# Zotero Cloud Bridge

A dashboard for connecting Zotero to Google Drive or OneDrive via WebDAV. It wraps rclone so users can configure attachment sync without using the command line.

**Status:** Project design. The setup below describes the intended workflow.

## What it syncs

The bridge handles **stored attachments in Zotero’s personal library**, such as PDFs. Zotero continues to sync library records, collections, tags, notes, and annotations through its own service. Group libraries are outside the scope of the bridge.

Zotero Desktop, the Word plugin, and the browser Connector can be used as usual. Keep `zotero.sqlite` in Zotero’s local data directory.

## Setup workflow

1. Open the dashboard and choose **Google Drive** or **OneDrive**.
2. Sign in to the provider in your browser and authorize access.
3. Select **Enable WebDAV**. The bridge creates the storage folder, chooses an available local port, and generates a WebDAV password.
4. In Zotero, go to **Settings → Sync → File Syncing**. Select **WebDAV** for **My Library** and paste the URL, username, and password from the dashboard.
5. Select **Verify Server** in Zotero. Sync one newly imported PDF, then use the dashboard to check that the upload reached the cloud provider.

Default settings cover local use. The **Advanced** panel is planned to expose the storage folder, preferred port, and WebDAV credentials.

## Architecture

- **Rust backend and web dashboard** handle provider setup, configuration, and service status.
- **Bundled rclone process** serves WebDAV and uploads attachments to the selected provider. The bridge keeps its own rclone configuration so it can coexist with an existing installation.
- **Zotero** connects to the WebDAV endpoint using the generated credentials. The cloud provider sign-in happens separately in the dashboard setup.

The default endpoint is local to the host. To sync from other devices, run the bridge on an always-on host and expose the WebDAV endpoint over HTTPS. Keep the dashboard and rclone control interface private.

## Roadmap

- [ ] Build the local dashboard and bundle rclone with the macOS app.
- [ ] Implement Google Drive authorization and verify attachment upload, download, update, and deletion.
- [ ] Add OneDrive and test the same sync workflow.
- [ ] Test multi-device sync over HTTPS, including Zotero mobile.

## References

- [Zotero sync documentation](https://www.zotero.org/support/sync)
- [rclone WebDAV documentation](https://rclone.org/commands/rclone_serve_webdav/)
