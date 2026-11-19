use eframe::egui;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_IMAGE: &str = "/opt/rustipc-installer/Image/yocto_docker.img";
const HELPER: &str = "/usr/local/sbin/rustipc-flash";

fn checked_image(path: impl AsRef<Path>) -> Option<(PathBuf, u64)> {
    let path = path.as_ref();
    if path.file_name()?.to_string_lossy() != "yocto_docker.img" {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.metadata().ok()?;
    (metadata.is_file() && metadata.len() > 0).then_some((canonical, metadata.len()))
}

fn locate_image() -> Option<(PathBuf, u64)> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("RUSTIPC_IMAGE") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from(DEFAULT_IMAGE));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Image/yocto_docker.img"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("Image/yocto_docker.img"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("Image/yocto_docker.img"));
        candidates.push(cwd.join("yocto_docker.img"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for suffix in [
            "Image/yocto_docker.img",
            "RustIPCInstaller/Image/yocto_docker.img",
            "Downloads/yocto_docker.img",
        ] {
            candidates.push(home.join(suffix));
        }
    }
    for path in candidates {
        if let Some(image) = checked_image(path) {
            return Some(image);
        }
    }
    let search = Command::new("find")
        .args([
            "/media",
            "/mnt",
            "-maxdepth",
            "8",
            "-type",
            "f",
            "-name",
            "yocto_docker.img",
            "-print",
            "-quit",
        ])
        .output()
        .ok()?;
    let found = String::from_utf8_lossy(&search.stdout).trim().to_string();
    if !found.is_empty() {
        checked_image(found)
    } else {
        None
    }
}

#[derive(Clone, Debug)]
struct Disk {
    path: String,
    size: u64,
    model: String,
    transport: String,
    reason: Option<String>,
}
#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<Device>,
}
#[derive(Deserialize)]
struct Device {
    path: Option<String>,
    name: String,
    size: Option<u64>,
    #[serde(rename = "type")]
    kind: String,
    tran: Option<String>,
    rm: Option<bool>,
    ro: Option<bool>,
    model: Option<String>,
    mountpoints: Option<Vec<Option<String>>>,
    children: Option<Vec<Device>>,
}
enum Event {
    Log(String),
    Finished(Result<(), String>),
}

struct Installer {
    disks: Vec<Disk>,
    selected: Option<usize>,
    logs: Vec<String>,
    busy: bool,
    confirmation: String,
    rx: Receiver<Event>,
    tx: Sender<Event>,
    last_scan: Instant,
    image_size: u64,
    image_path: Option<PathBuf>,
    image_path_input: String,
    startup_error: Option<String>,
}

impl Installer {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let image = locate_image();
        let image_size = image.as_ref().map(|(_, size)| *size).unwrap_or(0);
        let image_path_input = image
            .as_ref()
            .map(|(path, _)| path.display().to_string())
            .unwrap_or_default();
        let mut s = Self {
            disks: vec![],
            selected: None,
            logs: vec![],
            busy: false,
            confirmation: String::new(),
            rx,
            tx,
            last_scan: Instant::now() - Duration::from_secs(10),
            image_size,
            image_path: image.map(|(path, _)| path),
            image_path_input,
            startup_error: None,
        };
        s.refresh();
        s
    }
    fn refresh(&mut self) {
        self.last_scan = Instant::now();
        let output = Command::new("lsblk")
            .args([
                "--json",
                "--bytes",
                "--output",
                "NAME,PATH,SIZE,TYPE,TRAN,RM,RO,MODEL,MOUNTPOINTS",
            ])
            .output();
        let Ok(output) = output else {
            self.logs.push("Could not start lsblk.".into());
            return;
        };
        if !output.status.success() {
            self.logs
                .push("Disk discovery failed. Check the live system.".into());
            return;
        }
        let Ok(tree) = serde_json::from_slice::<Lsblk>(&output.stdout) else {
            self.logs
                .push("Could not parse lsblk disk information.".into());
            return;
        };
        let root = Command::new("findmnt")
            .args(["-n", "-o", "SOURCE", "/"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let root_chain = Command::new("lsblk")
            .args(["-s", "-n", "-o", "PATH,TRAN", &root])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let usb_root = root_chain
            .lines()
            .any(|line| line.split_whitespace().last() == Some("usb"));
        let root_names: Vec<String> = root_chain
            .lines()
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect();
        let mut found = Vec::new();
        fn has_mount(d: &Device) -> bool {
            d.mountpoints
                .as_ref()
                .is_some_and(|m| m.iter().any(|p| p.as_ref().is_some_and(|v| !v.is_empty())))
                || d.children
                    .as_ref()
                    .is_some_and(|cs| cs.iter().any(has_mount))
        }
        fn walk(d: &Device, root_names: &[String], image: u64, out: &mut Vec<Disk>) {
            if d.kind == "disk" {
                let path = d.path.clone().unwrap_or_else(|| format!("/dev/{}", d.name));
                let tran = d.tran.clone().unwrap_or_default().to_lowercase();
                let reason = if root_names.iter().any(|n| n == &path) {
                    Some("Installer USB / running system".into())
                } else if d.rm.unwrap_or(false) || tran == "usb" {
                    Some("Removable / USB device".into())
                } else if d.ro.unwrap_or(false) {
                    Some("Read-only device".into())
                } else if !["sata", "ata", "nvme"].contains(&tran.as_str()) {
                    Some("Unsupported disk transport".into())
                } else if d.size.unwrap_or(0) < image {
                    Some("Image does not fit".into())
                } else if has_mount(d) {
                    Some("Contains a mounted filesystem".into())
                } else {
                    None
                };
                out.push(Disk {
                    path,
                    size: d.size.unwrap_or(0),
                    model: d
                        .model
                        .clone()
                        .unwrap_or_else(|| "Internal disk".into())
                        .trim()
                        .to_string(),
                    transport: if tran.is_empty() {
                        "unknown".into()
                    } else {
                        tran
                    },
                    reason,
                });
            }
            if let Some(children) = &d.children {
                for c in children {
                    walk(c, root_names, image, out);
                }
            }
        }
        for d in &tree.blockdevices {
            walk(d, &root_names, self.image_size, &mut found);
        }
        self.startup_error = if self.image_size == 0 {
            Some("Yocto image not found. Copy yocto_docker.img to the app's Image folder, a mounted USB drive, or enter its path below.".into())
        } else if !usb_root {
            Some("Safety check stopped: the running root filesystem is not identified as USB-backed. Boot this installer from its USB drive before flashing.".into())
        } else {
            None
        };
        self.disks = found;
        self.selected = self.disks.iter().position(|d| d.reason.is_none());
    }
    fn start_flash(&mut self) {
        let Some(disk) = self
            .selected
            .and_then(|i| self.disks.get(i))
            .filter(|d| d.reason.is_none())
            .cloned()
        else {
            return;
        };
        let Some(image) = self.image_path.as_ref() else {
            return;
        };
        let image = image.display().to_string();
        self.busy = true;
        self.confirmation.clear();
        self.logs.push(format!(
            "Starting guarded flash of {} to {}.",
            image, disk.path
        ));
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Log(
                "Rechecking USB boot, target eligibility, capacity, mounts and swap as root…"
                    .into(),
            ));
            let mut child = match Command::new("sudo")
                .args(["-n", HELPER, "--target", &disk.path, "--image", &image])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Event::Finished(Err(format!(
                        "Could not start privileged flash helper: {e}"
                    ))));
                    return;
                }
            };
            let out = child.stdout.take();
            let err = child.stderr.take();
            let t1 = tx.clone();
            let reader = thread::spawn(move || {
                if let Some(out) = out {
                    use std::io::{BufRead, BufReader};
                    for line in BufReader::new(out).lines().flatten() {
                        let _ = t1.send(Event::Log(line));
                    }
                }
            });
            let t2 = tx.clone();
            let err_reader = thread::spawn(move || {
                if let Some(mut err) = err {
                    use std::io::Read;
                    let mut buffer = [0u8; 1024];
                    let mut pending = Vec::new();
                    loop {
                        match err.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                for byte in &buffer[..n] {
                                    if *byte == b'\r' || *byte == b'\n' {
                                        if !pending.is_empty() {
                                            let line = String::from_utf8_lossy(&pending)
                                                .trim()
                                                .to_string();
                                            if !line.is_empty() {
                                                let _ = t2.send(Event::Log(line));
                                            }
                                            pending.clear();
                                        }
                                    } else {
                                        pending.push(*byte);
                                    }
                                }
                            }
                        }
                    }
                    if !pending.is_empty() {
                        let _ = t2.send(Event::Log(
                            String::from_utf8_lossy(&pending).trim().to_string(),
                        ));
                    }
                }
            });
            let status = child.wait();
            let _ = reader.join();
            let _ = err_reader.join();
            let result = match status {
                Ok(s) if s.success() => Ok(()),
                Ok(s) => Err(format!("Flash helper exited with status {s}.")),
                Err(e) => Err(format!("Could not wait for flash helper: {e}")),
            };
            let _ = tx.send(Event::Finished(result));
        });
    }

    fn load_image_path(&mut self) {
        match checked_image(&self.image_path_input) {
            Some((path, size)) => {
                self.image_path_input = path.display().to_string();
                self.image_path = Some(path);
                self.image_size = size;
                self.logs.push("Yocto image loaded successfully.".into());
                self.refresh();
            }
            None => {
                self.image_path = None;
                self.image_size = 0;
                self.startup_error = Some(
                    "Could not open a non-empty file named yocto_docker.img at that path.".into(),
                );
            }
        }
    }
}

impl eframe::App for Installer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Log(line) => self.logs.push(line),
                Event::Finished(Ok(())) => {
                    self.busy = false;
                    self.logs.push("Flash completed and image SHA-256 verified. Power off, remove this USB, then start the installed system.".into());
                }
                Event::Finished(Err(e)) => {
                    self.busy = false;
                    self.logs.push(format!("FAILED: {e}"));
                }
            }
        }
        if self.last_scan.elapsed() > Duration::from_secs(15) && !self.busy {
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(10, 15, 28))).show(ctx, |ui| {
            let rect = ui.max_rect();
            let mut mesh = egui::Mesh::default();
            mesh.vertices.extend([
                egui::epaint::Vertex { pos: rect.left_top(), uv: egui::epaint::WHITE_UV, color: egui::Color32::from_rgb(8, 17, 34) },
                egui::epaint::Vertex { pos: rect.right_top(), uv: egui::epaint::WHITE_UV, color: egui::Color32::from_rgb(21, 26, 53) },
                egui::epaint::Vertex { pos: rect.right_bottom(), uv: egui::epaint::WHITE_UV, color: egui::Color32::from_rgb(11, 42, 54) },
                egui::epaint::Vertex { pos: rect.left_bottom(), uv: egui::epaint::WHITE_UV, color: egui::Color32::from_rgb(8, 25, 42) },
            ]);
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            ui.painter().add(egui::Shape::mesh(mesh));
            ui.painter().circle_filled(egui::pos2(rect.right() - 70.0, rect.top() + 50.0), 240.0, egui::Color32::from_rgba_unmultiplied(26, 180, 174, 12));
            ui.painter().circle_filled(egui::pos2(rect.left() + 80.0, rect.bottom() - 30.0), 220.0, egui::Color32::from_rgba_unmultiplied(72, 103, 220, 12));

            let content_size = egui::vec2(
                (rect.width() - 48.0).clamp(320.0, 940.0),
                (rect.height() - 32.0).clamp(520.0, 820.0),
            );
            let content_rect = egui::Rect::from_center_size(rect.center(), content_size);
            ui.allocate_ui_at_rect(content_rect, |ui| {
                ui.set_width(content_size.x);
                let vertical_pad = ((ui.available_height() - 650.0) * 0.5).max(8.0);
                ui.add_space(vertical_pad);
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new("R U S T I P C     /     DEVICE INSTALLER").size(13.0).strong().color(egui::Color32::from_rgb(105, 225, 208)));
                ui.add_space(9.0);
                ui.heading(egui::RichText::new("Install Yocto with Docker").size(38.0).strong().color(egui::Color32::WHITE));
                ui.add_space(5.0);
                ui.label(egui::RichText::new("A guided, verified installation for this machine").size(16.0).color(egui::Color32::from_rgb(173, 189, 211)));
                ui.add_space(24.0);

                egui::Frame::group(ui.style()).fill(egui::Color32::from_rgba_unmultiplied(17, 31, 49, 232)).stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 72, 92))).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("◈").size(40.0).strong().color(egui::Color32::from_rgb(90, 222, 203)));
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new("YOCTO WITH DOCKER").size(12.0).strong().color(egui::Color32::from_rgb(105, 225, 208)));
                            ui.label(egui::RichText::new("yocto_docker.img").size(22.0).strong().color(egui::Color32::WHITE));
                            if let Some(path) = &self.image_path {
                                ui.label(egui::RichText::new(format!("{}  ·  {} MiB", path.display(), self.image_size / 1024 / 1024)).size(12.0).color(egui::Color32::from_rgb(160, 178, 199)));
                            } else {
                                ui.label(egui::RichText::new("Image file not loaded").size(13.0).color(egui::Color32::from_rgb(255, 174, 117)));
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(!self.busy, egui::Button::new("↻  Rescan").min_size(egui::vec2(120.0, 38.0))).clicked() { self.refresh(); }
                        });
                    });
                    if self.image_path.is_none() {
                        ui.add_space(10.0);
                        ui.label("Enter the full path to the separately transferred image:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.image_path_input).desired_width((content_size.x - 170.0).max(150.0)).hint_text("/media/installer/USB/Image/yocto_docker.img"));
                            if ui.button("Load image").clicked() { self.load_image_path(); }
                        });
                        ui.label(egui::RichText::new("The app also searches its Image folder and mounted USB drives automatically.").small().color(egui::Color32::from_rgb(150, 169, 191)));
                    }
                });
                ui.add_space(14.0);

                egui::Frame::group(ui.style()).fill(egui::Color32::from_rgba_unmultiplied(17, 31, 49, 232)).stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 72, 92))).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("01").size(15.0).strong().color(egui::Color32::from_rgb(105, 225, 208)));
                        ui.label(egui::RichText::new("SELECT INTERNAL DRIVE").strong().color(egui::Color32::WHITE));
                    });
                    ui.add_space(8.0);
                    if self.disks.is_empty() { ui.label(egui::RichText::new("No eligible internal drives detected.").color(egui::Color32::from_rgb(173, 189, 211))); }
                    for (i, d) in self.disks.iter().enumerate() {
                        let enabled = d.reason.is_none() && !self.busy;
                        let label = format!("{}   {}   ·   {} GiB   ·   {}{}", d.model, d.path, d.size / 1024 / 1024 / 1024, d.transport, d.reason.as_ref().map(|r| format!("   /   {r}")).unwrap_or_default());
                        if ui.add_enabled(enabled, egui::RadioButton::new(self.selected == Some(i), label)).clicked() { self.selected = Some(i); }
                    }
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("USB drives, mounted drives, and the running installer drive are excluded.").small().color(egui::Color32::from_rgb(150, 169, 191)));
                });
                ui.add_space(14.0);

                egui::Frame::group(ui.style()).fill(egui::Color32::from_rgba_unmultiplied(17, 31, 49, 232)).stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 72, 92))).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("02").size(15.0).strong().color(egui::Color32::from_rgb(255, 190, 114)));
                        ui.label(egui::RichText::new("CONFIRM ERASE").strong().color(egui::Color32::WHITE));
                    });
                    ui.label("The selected drive will be completely erased. Type ERASE to continue.");
                    ui.horizontal(|ui| {
                        ui.add_enabled(!self.busy, egui::TextEdit::singleline(&mut self.confirmation).desired_width(250.0).hint_text("Type ERASE"));
                        let can_flash = !self.busy && self.startup_error.is_none() && self.image_path.is_some() && self.confirmation == "ERASE" && self.selected.and_then(|i| self.disks.get(i)).is_some_and(|d| d.reason.is_none());
                        let button = egui::Button::new(egui::RichText::new(if self.busy { "FLASHING…" } else { "FLASH YOCTO IMAGE" }).strong().color(egui::Color32::from_rgb(8, 24, 35))).fill(egui::Color32::from_rgb(89, 220, 195)).min_size(egui::vec2(250.0, 44.0));
                        if ui.add_enabled(can_flash, button).clicked() { self.start_flash(); }
                    });
                    if let Some(error) = &self.startup_error {
                        ui.add_space(5.0);
                        ui.colored_label(egui::Color32::from_rgb(255, 153, 136), error);
                    }
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("LIVE ACTIVITY").strong().color(egui::Color32::from_rgb(105, 225, 208)));
                    ui.label(egui::RichText::new("  ·  dd write and SHA-256 verification").small().color(egui::Color32::from_rgb(150, 169, 191)));
                });
                egui::Frame::group(ui.style()).fill(egui::Color32::from_rgba_unmultiplied(7, 13, 24, 220)).stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(39, 57, 77))).show(ui, |ui| {
                    egui::ScrollArea::vertical().stick_to_bottom(true).max_height(110.0).show(ui, |ui| {
                        if self.logs.is_empty() { ui.label(egui::RichText::new("Ready when you are.").monospace().color(egui::Color32::from_rgb(150, 169, 191))); }
                        for line in &self.logs {
                            let color = if line.starts_with("FAILED") || line.starts_with("ERROR:") { egui::Color32::LIGHT_RED } else { egui::Color32::from_rgb(205, 219, 233) };
                            ui.label(egui::RichText::new(line).monospace().size(12.0).color(color));
                        }
                    });
                });
                });
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustIPC Installer")
            .with_fullscreen(true)
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "RustIPC Installer",
        options,
        Box::new(|cc| {
            let mut style = (*cc.egui_ctx.style()).clone();
            style.visuals = egui::Visuals::dark();
            style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(17, 31, 49);
            style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(26, 44, 63);
            style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(39, 69, 87);
            style.visuals.selection.bg_fill = egui::Color32::from_rgb(32, 137, 133);
            cc.egui_ctx.set_style(style);
            Ok(Box::new(Installer::new()))
        }),
    )
}
