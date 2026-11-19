# RustIPC Installer

A full-screen Ubuntu kiosk for writing the bundled `Image/yocto_docker.img` to an internal SATA/ATA/NVMe drive. It is intended to boot from an Ubuntu Server installation on a USB pendrive. The desktop app runs as an unprivileged account; a small root-owned helper repeats all safety checks and performs the write.

## What the installer does

1. Shows the single image option, **Yocto with Docker**.
2. Requires the installer root filesystem to be identified as USB-backed.
3. Lists internal SATA/ATA/NVMe drives. USB/removable drives, the running installer drive, mounted drives, undersized drives and read-only drives are disabled.
4. Requires the operator to select a drive and type `ERASE`.
5. Rechecks the boot source and target as root, checks mounted filesystems, swap, capacity, and disk type, then writes with `dd`.
6. Shows `dd` progress in the kiosk and reads back the written image to compare its SHA-256 hash.

This overwrites the selected disk's partition table and data. When the helper reports success, power off and remove the installer pendrive before starting the newly flashed machine.

## Build on Ubuntu 26.04 Desktop (amd64)

The Ubuntu Server 26.04 LTS amd64 release is available from [Ubuntu's official download page](https://ubuntu.com/download/server). The app is a native X11 application; no browser or network connection is needed while flashing.

1. Open a terminal and install the build tools and native libraries:

   ```bash
   sudo apt update
   sudo apt install -y build-essential pkg-config libx11-dev libxcursor-dev \
     libxrandr-dev libxi-dev libxkbcommon-dev libgl1-mesa-dev curl git
   ```

2. Install Rust using rustup (the project requires Rust 1.85 or newer):

   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   . "$HOME/.cargo/env"
   rustup update stable
   rustc --version
   ```

3. Pull/clone this project on the desktop. Since the image is intentionally excluded from Git, transfer the approved image file separately and place it at `Image/yocto_docker.img`. Then confirm it is present:

   ```bash
   cd RustIPCInstaller
   test -s Image/yocto_docker.img
   sha256sum Image/yocto_docker.img
   ```

   Keep the hash with your release notes so that you can identify the exact image build later. The current image is 877,493,248 bytes (about 836 MiB), SHA-256 `519670607d201b11de0bcc20c3821495a608504e4be17455a4965844845665ad`.

4. Build the optimized executable:

   ```bash
   cargo build --release
   file target/release/rustipc-installer
   ```

   The executable is dynamically linked to system X11/OpenGL libraries, which the kiosk setup installs. Do not build it for Windows; build on Ubuntu amd64 for the Ubuntu kiosk.

## Make a bootable kiosk pendrive

Use two USB drives during preparation: a temporary Ubuntu installer drive (8 GB or larger) and the intended kiosk pendrive (8 GB, 16 GB or 64 GB). A third USB/data source is useful for transferring the built project to the kiosk, unless the kiosk has network access and can clone the project. The kiosk pendrive is a persistent Ubuntu installation, not a live session; ordinary live media is not persistent and must not be used for this workflow.

### 1. Install Ubuntu Server onto the kiosk pendrive

1. Download the current Ubuntu Server **26.04 LTS amd64** ISO from [ubuntu.com](https://ubuntu.com/download/server) and write it to the temporary installer drive with Rufus, balenaEtcher, or Startup Disk Creator. This temporary drive will be erased.
2. Insert both the temporary installer drive and the kiosk pendrive into the target PC. Boot from the temporary installer drive in UEFI mode.
3. In the Ubuntu Server installer, choose the minimal installation option if offered. Create a user named `installer` (or choose another name and pass that name to the setup script) with a password for local maintenance. On storage, select the **kiosk pendrive only** as the installation disk and create the EFI system and ext4 root partitions on that same drive. Check the selected disk model and size carefully. Do not select the internal disk. The USB installer and kiosk pendrive are separate drives.
4. Finish the installation, reboot, and choose the kiosk pendrive in the firmware boot menu. Remove the temporary installer drive. Confirm the machine boots to the installed Ubuntu shell from the kiosk pendrive.

The 8 GB pendrive has the least free space. Keep this installation minimal, do not add optional server roles, and allow enough free space for the image and logs. USB flash capacity is marketed in decimal GB; an 8 GB drive exposes roughly 7.45 GiB before partition/filesystem overhead. Check free space with `df -h /` before installing the payload. 16 GB or larger is recommended if the kiosk will be updated or retain logs.

### 2. Copy the built project to the kiosk

Use either a temporary data drive containing this project (including `target/release/rustipc-installer` and the separately transferred `Image/yocto_docker.img`) or clone/pull the repository on the kiosk and transfer the image there separately. If using a data drive, mount it and keep its path handy. The Ubuntu account created during installation must exist before running setup. Then run the setup script from the project directory:

```bash
cd /path/to/RustIPCInstaller
sudo bash scripts/install-kiosk.sh installer "$PWD"
sudo reboot
```

If transferring the project over the network, connect to a trusted network, clone the repository, verify `Image/yocto_docker.img` is present, and run the same commands. The script installs only the minimal Xorg/Openbox kiosk dependencies, copies the image and executable under `/opt/rustipc-installer`, configures a constrained passwordless sudo rule for the flash helper, and enables the full-screen kiosk on tty1. It does not compile on the kiosk.

After reboot, the kiosk starts automatically. The installer needs local disk utilities already provided by Ubuntu (`lsblk`, `findmnt`, `dd`, `blockdev`, `sha256sum`, `swapon`). It does not need SSH, network connectivity, or Docker to write the image.

### 3. Flash the target machine

1. Boot the target machine from the kiosk pendrive. Connect the disk to be reimaged internally (SATA/ATA or NVMe). The kiosk pendrive must remain connected for the entire flash.
2. Confirm the displayed image and internal drive model, path and size. Ensure no desired data is on that drive.
3. Type `ERASE` and choose **FLASH YOCTO IMAGE**. Follow the byte progress and hash verification in the activity panel. Do not power down or unplug either drive while writing or verifying.
4. After a success message, power off, remove the kiosk pendrive, and boot from the flashed disk.

If the kiosk says the USB boot check failed, the root filesystem was not positively mapped to a USB transport. Stop and boot from the installed USB pendrive again; do not weaken the helper's check. If a disk is disabled, resolve its displayed condition (for example, unmount it or choose a larger internal disk) and rescan.

## Maintenance

- Replace the image only with a release intended for this machine, then rebuild/re-copy the project payload and record its SHA-256.
- The flashing helper intentionally accepts only the fixed image path and a whole internal SATA/ATA/NVMe disk. It rejects USB and removable drives, active root disks, mounted filesystems, active swap, undersized targets, and read-only targets.
- Kiosk service logs can be inspected from a shell with `sudo journalctl -u rustipc-kiosk -b`. To leave the kiosk, use a keyboard on the machine and switch to another virtual terminal (for example, Ctrl+Alt+F2); the system remains an Ubuntu Server installation.
- This is a first release. Validate the exact hardware, firmware mode (UEFI/legacy), image bootability, screen resolution, and USB transport reporting on representative machines before production use. A valid byte-for-byte image write does not guarantee the image itself supports every device or boot mode.
