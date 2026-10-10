# Cloud sync

CreativeCraft keeps your projects folder in sync with a place of your choice, so the same work is
on every computer you use. Pick it in **Settings → Cloud sync**; **Test connection** checks it
without changing anything, and **Sync now** on the Files page (or `creativecraft-cli sync`) runs a
pass. It also syncs by itself every few minutes while CreativeCraft is open.

## Where you can sync to

| Choice | What it covers | You need |
|---|---|---|
| **Google Drive** | Google Drive, through Google's API | A free Google Cloud OAuth client; see [google-drive.md](google-drive.md) |
| **WebDAV / NAS** | Nextcloud, ownCloud, Synology, QNAP, TrueNAS, OpenMediaVault, pCloud, Koofr, Yandex Disk and other WebDAV servers | The folder's WebDAV address, a user name and (app) password |
| **S3 storage** | AWS S3, Backblaze B2, Cloudflare R2, Wasabi, MinIO, Garage, Storj and other S3-compatible storage, including self-hosted on a NAS | Endpoint, region, bucket, access key and secret key |
| **rclone** | Anything your own [rclone](https://rclone.org) can reach: Dropbox, OneDrive, Box, pCloud, Mega, Proton Drive, SFTP and SMB shares, and 70-odd more | rclone installed, and a remote set up with `rclone config` |
| **A folder** | A folder another sync app already backs up (Google Drive for desktop, Dropbox, OneDrive, iCloud Drive, Syncthing…), or a NAS share mounted on this computer (SMB, NFS) | Nothing else |

Settings has templates for the common addresses (pick your server under **Templates**, then fill in
the parts in `<…>`).

### NAS setups

Most NAS boxes can do several of these; any one works:

- **Mount the share** (SMB/NFS, as your OS does it) and choose **A folder**. Simplest on a home
  network.
- **WebDAV**: Synology has a *WebDAV Server* package (HTTPS on port 5006 by default); QNAP, TrueNAS
  and OpenMediaVault have WebDAV built in or as a plug-in; Nextcloud on a NAS is WebDAV too.
- **S3**: MinIO or Garage running on the NAS (often port 9000). Turn **Path-style addresses** on.
- **SFTP or SMB through rclone**, for a NAS you reach over SSH or from outside your network.

A plain `http://` address is accepted for servers on your own network; it isn't encrypted, so
Settings warns about it. Use `https://` for anything reached over the internet.

## How your work is kept safe

- **Three-way sync.** CreativeCraft remembers the state after the last sync, so it knows which side
  changed. If the same file changed on both, both versions are kept: the other one is saved next
  to yours as `name (cloud copy for <computer>).ext`.
- **Deletions go to a trash, never away.** A file deleted on one computer is moved to
  `.creativecraft/trash/` in the projects folder on the others, and to the store's trash in the
  cloud (Drive's trash, or a `.creativecraft-trash` folder for WebDAV, S3, rclone and folders).
- **Downloads are checked before they replace anything.** Each download lands in
  `.creativecraft/incoming/`, is compared with the file's checksum where the store has one, and only
  then is renamed into place. A damaged download is reported and fetched again next sync. S3
  uploads carry a checksum the server verifies (files over 64 MB go up in parts, each checked
  and retried on its own; an upload that can't finish is cancelled on the server); rclone
  uploads are checked after the copy.
- **Edits made during a sync win.** A file you changed while a sync was running is left alone and
  synced next time.
- **One sync at a time**, even when the window, the CLI and an agent all ask for one.
- **Retries.** Rate limits, server errors and dropped connections are retried a few times with
  growing pauses (honouring the server's `Retry-After`). Other failures are reported with the
  reason, and the file is tried again next pass.

## Fast on big folders

Local files are only re-hashed when they changed (size, modification time and, on Linux and
macOS, the change time that copying tools can't fake). For stores that report checksums (Drive,
S3, many rclone remotes) nothing is downloaded to compare; for stores that don't (WebDAV, some
rclone remotes) a file changed by another app is fetched once, hashed, and that same copy is used
if it needs downloading. Changes made by CreativeCraft on another computer are recognised without
downloading.

## Files not to sync

Put patterns in `.creativecraftignore` in the projects folder (Settings → Cloud sync → **Edit
ignore rules** creates one), one per line:

```text
*.mov          every .mov file
renders/       every folder called renders, with everything inside
/exports/      only the exports folder at the top
cache/**/tmp   tmp anywhere under cache
!keep.mov      sync this one after all
```

The file itself syncs, so all your computers share the rules. Files that match are left alone on
both sides. CreativeCraft always skips its own `.creativecraft/` folder, `.git`, editor temp files
and OS litter (`.DS_Store`, `Thumbs.db`).

## Passwords and keys

WebDAV passwords, S3 secret keys and your Google Drive sign-in are kept in your computer's keychain:
the macOS Keychain, the Windows Credential Manager, or your desktop's keyring on Linux (GNOME
Keyring, KWallet, KeePassXC… through Secret Service). Where there's none (a computer without a
desktop session, or a portable install with `CREATIVECRAFT_HOME`), they go in `secrets.json` in
CreativeCraft's config folder, readable only by you. Never in `settings.json`. Settings → General
says which is used. Each setup keeps its own, so switching between two
servers doesn't lose either password. Prefer an app password (Nextcloud: Settings → Security) or an
S3 key limited to one bucket. rclone keeps its own accounts; CreativeCraft never sees them.

## Troubleshooting

| What you see | What to do |
|---|---|
| "the server didn't accept the user name or password" | Check them; for Nextcloud with two-factor sign-in, make an app password. |
| "the folder above … doesn't exist on the server" | The address is wrong above the sync folder. For Nextcloud it's `…/remote.php/dav/files/<user name>/<folder>`. |
| "the secret key is wrong" / "the access key ID isn't known" | Copy the key pair again, without spaces. |
| "there's no bucket with that name" | Create the bucket first, or check the endpoint's region. |
| "the region doesn't match" / "the bucket is in another region" | Use the bucket's region in both the endpoint and the region field. |
| "rclone isn't installed" | Install it from rclone.org, run `rclone config`, then **Look again** in Settings. |
| `… (cloud copy for …)` files | Two computers changed the same file between syncs. Both versions were kept; keep the one you want. |
| A file you deleted came back | It was changed on another computer before that one saw the deletion. Changes win over deletions, so no work is lost. |
| "changed locally during sync; will retry" | You were editing that file while it synced. It's synced on the next pass. |

`creativecraft-cli sync --test` checks the connection, `sync --preview` shows what a sync would do
without doing it, and `sync` prints the reason for any file that failed.
