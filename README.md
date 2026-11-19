# Zotero Cloud Bridge

A dashboard for connecting Zotero to Google Drive or OneDrive via WebDAV. It wraps rclone so users can configure attachment sync without using the command line.

The local dashboard supports Google Drive and OneDrive setup and starts an authenticated WebDAV server on loopback.

## Run on macOS

Run `cargo run --locked` from the repository root to launch the dashboard. Cargo creates a temporary macOS app bundle and uses the verified rclone v1.76.0 archive cached under `target/rclone`. The first run downloads the archive. Pass `-- --no-browser` to print the dashboard URL without opening it.

Run `scripts/bundle-macos.sh` to build `target/Zotero Cloud Bridge.app` for use outside Cargo. Both workflows run rclone from `Contents/Resources/rclone` inside their app bundle.

Bridge keeps one verified connection in `~/Library/Application Support/Zotero Cloud Bridge/rclone.conf`. Setup uses rclone's built-in OAuth clients. Google Drive uses the read/write `drive` scope for My Drive, and the dashboard asks before using rclone's shared Google client ID.

## What it syncs

The bridge handles **stored attachments in Zotero’s personal library**, such as PDFs. Zotero continues to sync library records, collections, tags, notes, and annotations through its own service. Group libraries are outside the scope of the bridge.

Zotero Desktop, the Word plugin, and the browser Connector can be used as usual. Keep `zotero.sqlite` in Zotero’s local data directory.

## Setup workflow

1. Open the dashboard and choose **Google Drive** or **OneDrive**.
2. Sign in to the provider in your browser and authorize access.
3. Turn on the **WebDAV** switch. Bridge prepares the `Zotero Cloud Bridge/zotero` folder and displays the URL and credentials when the server is ready.
4. In Zotero, go to **Settings → Account → File Syncing** (called **Sync** in older versions). Select **WebDAV** for **My Library** and enter the URL, username, and password from the dashboard. If Zotero displays a separate protocol selector, choose **http** and enter the host and port. Zotero adds `/zotero/` automatically.
5. Select **Verify Server**. In Bridge, click **Capture baseline** before importing a test PDF as a stored attachment. Sync in Zotero, then click **Check cloud folder** in the same dashboard tab. Bridge reads the folder directly from the cloud and flags newly observed paths as candidates for the test upload.

Turn off the **WebDAV** switch to stop the service or before changing the cloud provider. Bridge also stops WebDAV when it quits. Turn on the switch after reopening Bridge. Bridge reuses the saved password and the previous port when available. When the port changes, the dashboard prompts you to update Zotero's URL.

Default settings cover local use. Open **Advanced** in the WebDAV section to edit the storage folder, port mode, or credentials. Applying settings can restart WebDAV. Attachments stay in their original folders when you change the storage location.

## Architecture

- **Rust backend and web dashboard** handle provider setup, configuration, and service status.
- **Bundled rclone process** serves WebDAV and uploads attachments to the selected provider. The bridge keeps its own rclone configuration so it can coexist with an existing installation.
- **Zotero** connects to the WebDAV endpoint using the generated credentials. The cloud provider sign-in happens separately in the dashboard setup.

The default endpoint is local to the host. To sync from other devices, run Bridge on an always-on host and follow the [HTTPS setup guide](docs/5.multi-device-https.md) to expose the Zotero WebDAV route through Caddy.

## Roadmap

- [ ] Build the local dashboard and bundle rclone with the macOS app.
- [ ] Implement Google Drive authorization and verify attachment upload, download, update, and deletion.
- [ ] Add OneDrive and test the same sync workflow.
- [ ] Test multi-device sync over HTTPS, including Zotero mobile.

## References

- [Zotero sync documentation](https://www.zotero.org/support/sync)
- [rclone WebDAV documentation](https://rclone.org/commands/rclone_serve_webdav/)
