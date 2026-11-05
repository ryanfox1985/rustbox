# rustbox

A small HTTP API and web UI for switching Raspberry Pi GPIO pins on and off. Pin states are stored in SQLite and restored at startup, so outputs come back after a reboot.

## Web UI

Open `http://<pi>:8080/` in a browser to list, add, edit, toggle and delete pins. The pages and stylesheet are built into the binary. Form posts from other sites are rejected with `403`.

## API

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/health` | Health check |
| `GET` | `/pins` | List pins |
| `POST` | `/pins` | Create a pin |
| `GET` | `/pins/{id}` | Show a pin |
| `PATCH` / `PUT` | `/pins/{id}` | Update a pin |
| `DELETE` | `/pins/{id}` | Delete a pin |

A pin has a `name`, a `pin_pi` (BCM GPIO number) and a `value` (`true` drives the pin high, `false` low). The body can be bare or wrapped in `"pin"`:

```sh
curl -X POST localhost:3000/pins -H 'Content-Type: application/json' \
  -d '{"name": "relay", "pin_pi": 17, "value": true}'
```

Pins that aren't on the board's header return `422`. Deleting a pin leaves its output at the last level.

## Google Home (Matter)

rustbox is also a Matter bridge: each pin shows up in Google Home as an outlet you can switch from the app or by voice. Control stays on your network, through a Google Nest speaker or display acting as the Matter hub.

rustbox uses the Matter test certificates, so Google Home only accepts it as a development device:

1. In the [Google Home Developer Console](https://console.home.google.com/), create a project with a Matter integration for Vendor ID `0xFFF1` and Product ID `0x8001`. Use the same Google account as the Google Home app.
2. Set `MATTER_PASSCODE` to your own 8-digit code. The default, `20202021`, is public, so anyone on your network could pair while pairing is open.
3. Start rustbox. Until it's paired, it logs a QR code and an 11-digit pairing code:

   ```sh
   journalctl -u rustbox | grep -E 'PairingCode|SetupQRCode'
   ```

4. In the Google Home app, tap **Add** → **Device** → **Matter-enabled device**, then scan the QR code or enter the pairing code. Accept the warning that the device isn't certified.

Pairing is open for 15 minutes after startup; restart rustbox to reopen it. Changes from the API, the web UI and Google Home all go through the same code, so each sees the others'. Pins created or deleted later are added to or removed from the bridge. A pin is named after its `name`, or `Pin <id>` without one. Up to 64 pins are bridged.

Pairings and keys are kept in `MATTER_DIR`. To start over, remove the bridge in Google Home, stop rustbox and delete that directory.

Matter needs UDP ports 5540 and 5353 (mDNS) and an IPv6 address on the Pi's network interface. In Docker it only works with host networking; otherwise set `MATTER=off`.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `PORT` | `3000` | JSON API port |
| `UI_PORT` | `8080` | Web UI port |
| `DATABASE_URL` | `sqlite:rustbox.db` | SQLite database, created if missing |
| `RUST_LOG` | `info` | Log filter |
| `MATTER` | on | `off` disables Google Home / Matter |
| `MATTER_DIR` | `matter` | Matter pairings and keys, created if missing |
| `MATTER_PASSCODE` | `20202021` | Matter setup code |
| `MATTER_DISCRIMINATOR` | `3840` | Matter discriminator (0–4095), part of the pairing code |

## Install on a Raspberry Pi

Each release ships a static binary per architecture plus a systemd unit. Pick the binary that matches `uname -m` on the Pi:

| `uname -m` | Binary |
| --- | --- |
| `armv6l` | `rustbox-armv6` (Pi 1, Zero) |
| `armv7l` | `rustbox-armv7` |
| `aarch64` | `rustbox-aarch64` |

Download both files from the latest release (replace `OWNER` with the GitHub account) and install them:

```sh
base=https://github.com/OWNER/rustbox/releases/latest/download
curl -fL -O "$base/rustbox-armv7" -O "$base/rustbox.service"

sudo install -m 755 rustbox-armv7 /usr/local/bin/rustbox
sudo install -m 644 rustbox.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now rustbox
```

The service starts at boot, restarts on failure, and runs as an unprivileged user in the `gpio` group. The database lives in `/var/lib/rustbox/`. To change settings, run `sudo systemctl edit rustbox` and override the `Environment=` lines.

Check it with:

```sh
systemctl status rustbox
journalctl -u rustbox -f
```

To upgrade, install the new binary over `/usr/local/bin/rustbox` and run `sudo systemctl restart rustbox`.

## Development

```sh
cargo run
```

Off a Pi, GPIO is unavailable: the API still works, but pin changes don't reach hardware.

To run it in Docker instead:

```sh
docker compose up --build
```

## Building and releasing

Cross-compile all Pi binaries and the systemd unit into `dist/`:

```sh
docker build -f Dockerfile.pi -o dist .
```

CI builds on every push. Pushing a `v*` tag publishes a GitHub release with the files from `dist/`.
