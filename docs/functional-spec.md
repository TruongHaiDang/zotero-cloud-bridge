# Zotero Cloud Bridge technical design

**Status** — Draft for review before implementation. The scope comes from [README.md](../README.md). Open decisions are listed below.

## 1. Scope

Bridge provides a WebDAV endpoint for Zotero to sync stored attachments in a personal library (**My Library**) to Google Drive or OneDrive. The acceptance workflow uses a newly imported PDF.

Zotero continues to sync library records, collections, tags, notes, and annotations through its own service. Group libraries and linked attachments are outside scope. Keep `zotero.sqlite` in Zotero's local data directory. Zotero Desktop, the Word plugin, and the browser Connector continue to work through their existing workflows.

The Rust backend manages configuration, cloud provider authorization, and service state. The dashboard provides the user interface. The rclone process bundled with the macOS app serves WebDAV and accesses cloud storage using Bridge's dedicated configuration. Zotero uses the WebDAV credentials provided by Bridge. Google Drive and OneDrive authorization takes place in the browser.

## Proposed code layout

The repository currently has a starter program in `src/main.rs`. The module names, types, and function signatures below and in the linked documents are proposals for review.

| Module | Responsibility |
| --- | --- |
| `main` | Initialize configuration, services, and the local dashboard. |
| `dashboard` | Handle UI actions, call services, and return state. |
| `provider` | Manage authorization sessions and the active remote. |
| `rclone` | Build commands, run rclone with its dedicated configuration, and report process errors to services. |
| `webdav` | Manage the WebDAV process and endpoint state. |
| `settings` | Read, validate, and save Bridge configuration. |
| `sync_check` | Query objects directly from the remote to verify sync. |

Keep Bridge and rclone settings in separate files. Bridge stores the remote reference, folder, port, and WebDAV settings using a secret storage mechanism still to be decided. The rclone file stores remote configuration and tokens. Save Bridge settings to a temporary file in the same directory, then rename it into place to avoid leaving a partially written file.

## 2. Feature designs

1. [Connect a cloud provider](1.provider-connection.md)
2. [Enable WebDAV](2.enable-webdav.md)
3. [Configure Zotero and verify sync](3.zotero-sync.md)
4. [Advanced settings](4.advanced-settings.md)
5. [Sync across devices over HTTPS](5.multi-device-https.md)

## 3. Implementation order from the README

1. Build the local macOS dashboard and bundle rclone with the app.
2. Add Google Drive, enable WebDAV, run **Verify Server**, and verify uploads, downloads, updates, and deletions.
3. Add OneDrive and repeat the Google Drive acceptance tests.
4. Test multi-device sync over HTTPS, including Zotero mobile.

## 4. Open decisions

- **Cloud connections.** Should Bridge use one provider at a time or support multiple configurations in parallel? Should users migrate existing files when switching providers?
- **Storage folder.** What is the default name? Can users point Bridge to an existing folder or one another Zotero installation already uses? Should Bridge support moving files when the folder changes?
- **Service lifecycle.** Should WebDAV start when the app opens, keep running in the background after the dashboard closes, or start on macOS login? If the saved port is in use at startup, should the service preserve the URL or select a new port automatically?
- **Authorization and secrets.** Decide how to register OAuth applications for each provider, what scopes to request, and how to store configurations containing tokens and WebDAV credentials on macOS.
- **Dashboard verification.** Decide whether to list the files visible on the remote or guide users through verification of a specific PDF. Define how to identify the test upload when WebDAV object names differ from the PDF filename.
- **Advanced settings and HTTPS milestones.** Should Advanced settings ship with Google Drive or later? Should Bridge document server-side HTTPS setup or support it from the dashboard?

## 5. References

- [Zotero Sync](https://www.zotero.org/support/sync)
- [rclone serve webdav](https://rclone.org/commands/rclone_serve_webdav/)
