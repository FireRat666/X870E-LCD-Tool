# X870E Extreme LCD Tool

A native, high-performance Linux driver, CLI daemon, and desktop GUI written in **Rust** to control, customize, flash images, and stream real-time hardware telemetry to the 5-inch onboard color LCD panel (`0b05:1c83`) on the ASUS ROG Crosshair X870E Extreme motherboard.

---

## Features

- **Rootless Access**: Seamlessly control the panel without `sudo` via clean udev rules.
- **Hardware Monitor Modes & Layouts**:
  - `Single`: 1 large sensor dial.
  - `Dual`: 2 sensor gauges.
  - `Triple`: 3 sensor gauges.
  - `Multi`: 5 sensor gauges (requires Theme Style 3).
  - **Visual Themes**: Switch between theme styles (**Styles 1, 2, 3, and 4**).
  - **Custom Sensor Assignments**: Choose what appears in each slot (CPU Package Temp, CPU Core Frequency, CPU Utilization %, GPU Temp, GPU Utilization %, RAM Usage, RAM Usage %, CPU Model Name).
- **Power & Safety Controls**:
  - **Standby Wallpaper ("When system is in sleep, hibernate or soft off states")**: Hardware-level setting (`0x5c` byte 16) that keeps the default wallpaper illuminated on 5V standby power when the PC enters sleep, hibernate, or soft-off.
  - **Temperature Warning Alerts**: Configurable temperature warning thresholds (75°C, 80°C, 85°C, 90°C, 95°C, 100°C) triggering visual alerts when components overheat.
- **Multi-Slot Image Storage & Display**:
  - **Built-in Default Wallpapers (ROM Presets 0..=5)**: Instantly switch between all 6 factory built-in wallpapers (Presets 0, 1, 2, 3, 4, and 5).
  - **Custom Flash Storage Slots (Slots 0..=N)**: Upload and store multiple custom images into the motherboard's 64MB SPI flash.
  - **Rock-Solid Hardware Safety**: Automatically scales, crops, and encodes to the panel's native **720 × 1280** resolution using standard JFIF YUV 4:2:0 JPEG with component IDs (1, 2, 3), preventing STM32H7 hardware decoder faults and black screens.
  - **Lockstep Hardware ACKs**: Every 4096-byte chunk waits for controller SPI flash write acknowledgment (`ee 14`) and finalize confirmation (`ee 13 00 ff`).
  - **Erase Slot**: Clear custom images from specific flash slots and restore factory defaults.
- **Built-in ROG Animations**: Switch between built-in ROG animation presets (`Preset 0` and `Preset 1`).
- **Backlight Brightness**: Smooth brightness adjustment from 0% to 100% with quick preset buttons.
- **Desktop GUI (`x870e-lcd-gui`)**: Sleek ROG-themed desktop interface built with `egui` featuring real-time 720x1280 screen previews, live sensor slot mapping, asynchronous non-blocking image flashing worker, and dedicated power/safety controls.
- **Headless CLI (`x870e-lcd`)**: Lightweight binary for scripting, keybinds, and running background systemd telemetry services.

---

## Installation

### Option A: Precompiled Release Binaries

Prebuilt standalone Linux binaries are available in the [GitHub Releases](../../releases) section:
1. Download `x870e-lcd-gui` and `x870e-lcd`.
2. Make them executable:
   ```bash
   chmod +x x870e-lcd-gui x870e-lcd
   ```
3. (Optional) Copy them to your user bin directory:
   ```bash
   mkdir -p ~/.local/bin
   cp x870e-lcd-gui x870e-lcd ~/.local/bin/
   ```

---

### Option B: Building from Source

#### 1. Install Build Dependencies

##### Arch Linux / CachyOS / Manjaro:
```bash
sudo pacman -S --needed base-devel rust cargo libusb pkgconf
```

##### Ubuntu / Debian / Pop!_OS:
```bash
sudo apt update
sudo apt install -y build-essential cargo rustc libusb-1.0-0-dev libudev-dev pkg-config libxkbcommon-dev
```

##### Fedora / RHEL:
```bash
sudo dnf install -y gcc gcc-c++ cargo rust libusb1-devel systemd-devel pkgconf-pkg-config libxkbcommon-devel
```

#### 2. Compile

Clone this repository and compile with Cargo in release mode:

```bash
git clone https://github.com/<your-username>/x870e-lcd-tool.git
cd x870e-lcd-tool

cargo build --release
```

The compiled binaries will be generated at:
- **GUI Application**: `target/release/x870e-lcd-gui`
- **CLI Tool**: `target/release/x870e-lcd`

#### 3. (Optional) Install System-Wide
```bash
sudo install -Dm755 target/release/x870e-lcd-gui /usr/local/bin/
sudo install -Dm755 target/release/x870e-lcd /usr/local/bin/
```

---

## Device Permissions (Udev Rules)

To control the LCD panel and upload images without needing `sudo`, install the included udev rule:

```bash
sudo cp udev/99-x870e-lcd.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Ensure your user belongs to the `plugdev` group (if used by your distribution):
```bash
sudo usermod -aG plugdev $USER
```

---

## Using the Desktop GUI

Launch the graphical interface:

```bash
./target/release/x870e-lcd-gui
```

### Features & Tabs:
- **Display & Modes**:
  - Turn Screen ON / OFF.
  - Adjust backlight brightness (0 - 100%) or use quick buttons (25%, 50%, 75%, 100%).
  - Switch between Built-in Animations (`Preset 0` and `Preset 1`).
  - Switch between Image Slots (`Slot 0` Factory Default, `Slot 1` Custom, `Slot 2`, `Slot 3`).
- **Hardware Telemetry**:
  - Layout selector: Single Gauge (1 Metric), Dual Info (2 Metrics), Triple Info (3 Metrics), or Multi Info (5 Metrics).
  - Theme selector: Choose between Theme Styles 1 through 6 (Multi Info automatically uses Style 3).
  - Slot Metric Assigners: Configure which sensor appears in each slot with live preview values.
  - **Start Live Streaming**: Stream live stats directly to the LCD screen.
- **Custom Image (Flash)**:
  - Click **📂 Browse Image File...** to pick any PNG, JPG, WebP, BMP, or GIF image.
  - Choose scaling: `Cover` (center-crop), `Fit` (letterbox with black bars), or `Stretch`.
  - Adjust JPEG encoding quality (default: 92).
  - Select target destination slot (`Slot 0..=4`).
  - Click **⚡ Flash Image to Motherboard LCD** to upload asynchronously.
  - Click **🗑 Erase Slot** to wipe a specific custom slot and restore the factory wallpaper.

---

## Using the Command-Line Interface (CLI)

The CLI binary is located at `target/release/x870e-lcd`.

### 1. Check Connection
```bash
./target/release/x870e-lcd info
```

### 2. Multi-Slot Custom Image Flashing & Switching
```bash
# Upload an image to Custom Flash Slot 0
./target/release/x870e-lcd set-image /path/to/picture.png --slot 0 --fit cover --quality 90

# Upload a second image to Custom Flash Slot 1
./target/release/x870e-lcd set-image /path/to/second.png --slot 1 --fit cover --quality 90

# Switch active screen to Custom Slot 0 or Slot 1
./target/release/x870e-lcd mode custom-slot --index 0
./target/release/x870e-lcd mode custom-slot --index 1
```

### 3. Factory Default Wallpapers (ROM Presets 0..=5)
```bash
# Switch between the 6 built-in ASUS ROG wallpapers
./target/release/x870e-lcd mode default-wallpaper --index 0   # Default Wallpaper Preset 0
./target/release/x870e-lcd mode default-wallpaper --index 1   # Default Wallpaper Preset 1
./target/release/x870e-lcd mode default-wallpaper --index 2   # Default Wallpaper Preset 2
./target/release/x870e-lcd mode default-wallpaper --index 3   # Default Wallpaper Preset 3
./target/release/x870e-lcd mode default-wallpaper --index 4   # Default Wallpaper Preset 4
./target/release/x870e-lcd mode default-wallpaper --index 5   # Default Wallpaper Preset 5
```

### 4. Sleep & Standby Wallpaper Configuration
```bash
# Enable: Keep default wallpaper lit on 5V standby power during PC sleep/hibernate/soft-off
./target/release/x870e-lcd standby-wallpaper true

# Disable: Turn off LCD completely when PC sleeps
./target/release/x870e-lcd standby-wallpaper false
```

### 5. Erase Custom Flash Memory
```bash
# Wipes custom image from a specific SPI flash slot (e.g. slot 0) and restores factory wallpaper
./target/release/x870e-lcd erase --slot 0
```

### 6. Switch Operating Modes
```bash
# Built-in Animations (0 or 1)
./target/release/x870e-lcd mode animation --index 0
./target/release/x870e-lcd mode animation --index 1

# Hardware Monitor Layouts & Themes (Theme 1, 2, 3, or 4!)
./target/release/x870e-lcd mode monitor --layout single --theme 1
./target/release/x870e-lcd mode monitor --layout dual --theme 2
./target/release/x870e-lcd mode monitor --layout triple --theme 4
./target/release/x870e-lcd mode monitor --layout multi --theme 3

# Turn Screen Off
./target/release/x870e-lcd mode off
```

### 7. Adjust Backlight Brightness
```bash
./target/release/x870e-lcd brightness 100
./target/release/x870e-lcd brightness 50 --standby-wallpaper true
```

### 8. Run Telemetry Daemon with Temperature Warning Alerts
```bash
# Stream Triple Info with visual alert if CPU/GPU reaches or exceeds 80°C
./target/release/x870e-lcd monitor --layout triple --theme 2 --temp-warning 80 --interval 0.8

# Stream Multi Info (5 gauges) every 1.0 second
./target/release/x870e-lcd monitor --layout multi --theme 3 --interval 1.0
```

---

## Automatic Background Service (systemd)

To keep the screen continuously updated with live telemetry in the background:

1. Create a user service file at `~/.config/systemd/user/x870e-lcd.service`:

```ini
[Unit]
Description=ASUS ROG Crosshair X870E LCD Telemetry Service
After=default.target

[Service]
ExecStart=%h/.local/bin/x870e-lcd monitor --layout multi --theme 3 --interval 1.0
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
```

2. Enable and start the service:
```bash
systemctl --user daemon-reload
systemctl --user enable --now x870e-lcd.service
```

3. Check service status or logs:
```bash
systemctl --user status x870e-lcd.service
journalctl --user -u x870e-lcd.service -f
```

---

## Protocol Architecture & Notes

- **USB Device**: Vendor ID `0x0b05`, Product ID `0x1c83`.
- **Interface 0 (Vendor-Specific Bulk)**:
  - Bulk Endpoint `0x02` OUT streams 4096-byte blocks of JFIF baseline JPEG data directly to the onboard microcontroller's SPI flash memory.
- **Interface 1 (HID Command Channel)**:
  - Report ID `0xEC`: 64-byte payload controlling mode selection (`0x51`), visual themes & layout (`0x52`), telemetry text slots (`0x53`), backlight brightness (`0x5C`), and upload handshake headers (`0x71`, `0x72`, `0x73`, `0x7F`).
  - Report ID `0xEE`: Device ACK and flash commit status notifications on Endpoint `0x81` IN.
- **Hardware Notes**:
  - The motherboard firmware only possesses Multi Info gauge graphical assets for **Theme Style 3**. Styles 1, 2, 4, 5, 6 are designed for Single, Dual, and Triple gauge layouts.

---

## License

This project is licensed under the **GNU General Public License v3.0 (GPL-3.0-or-later)**. See [LICENSE](LICENSE) for details.
