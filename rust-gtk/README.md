# Claude Usage Monitor (Rust+GTK3)

## Build & Run

```bash
cargo run --release
```

## Install (.deb)

```bash
cargo build --release
dpkg-deb --build pkg claude-usage-monitor_0.1.0_amd64.deb
sudo dpkg -i claude-usage-monitor_0.1.0_amd64.deb
```

Autostart enabled by default. Disable: `sudo rm /etc/xdg/autostart/claude-usage-monitor.desktop`

## Controls

- **LMB drag** — move
- **RMB** — popup with details
- **Scroll** — resize
