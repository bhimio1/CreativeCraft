# Google Drive sync

CreativeCraft can keep your projects folder in sync with Google Drive, so the same work is on every
computer you use. This page explains what it does and walks through setting it up.

## How it works

- **One folder.** Everything goes into a Drive folder called **CreativeCraft Projects**, laid out
  exactly like your local projects folder (real Drive folders, so it looks right on
  drive.google.com).
- **Only its own files.** CreativeCraft asks Google for the `drive.file` permission, which lets it
  see and change only files it created. It can't read the rest of your Drive. This also means a
  file you upload to that folder through the Drive website won't sync down; save it into the
  local projects folder instead.
- **Nothing is lost.** Sync is three-way: it remembers the last synced state, so it knows which
  side changed. If the same file changed on both sides, both versions are kept; the other one is
  saved next to yours as `name (cloud copy for <computer>).ext`. A deletion is passed on as a
  move to the trash: Drive's own trash in the cloud, `.creativecraft/trash/` in your projects
  folder on other computers.
- **When it runs.** Every few minutes while CreativeCraft is open (Settings → Cloud sync →
  Sync automatically), or when you click **Sync now** on the Files page, or with
  `creativecraft-cli sync`.
- **Your sign-in stays on your computer.** CreativeCraft signs in through your browser; it never
  sees your Google password. The resulting access token is stored in the config folder
  (`google-token.json`, readable only by you).

## Why it needs setting up

Google only lets an app use the Drive API through an **OAuth client** that someone has registered
in Google Cloud. CreativeCraft doesn't ship one built in, so whoever runs or packages it creates a
client once (about ten minutes, free) and enters its ID and secret. If someone gave you a build
of CreativeCraft with Drive already working, skip to [Sign in](#6-sign-in).

> **Don't have a Google Cloud account or don't want one?** [Cloud sync](cloud-sync.md) also works
> with WebDAV, S3 and rclone, or use **Settings → Cloud sync → A folder** instead and point it at the folder that Google Drive for desktop (or Dropbox,
> OneDrive, Nextcloud…) already syncs. You get the same conflict-safe sync with no setup.

## Setup

### 1. Create a Google Cloud project

1. Go to <https://console.cloud.google.com/> and sign in with a Google account.
2. Open the project picker at the top and choose **New project**. Any name works, for example
   "CreativeCraft". Create it and make sure it's selected.

### 2. Turn on the Google Drive API

1. Go to **APIs & Services → Library** (or open
   <https://console.cloud.google.com/apis/library/drive.googleapis.com>).
2. Find **Google Drive API** and click **Enable**.

### 3. Set up the consent screen

This is the page Google shows when you sign in. In the console it's under **Google Auth
Platform** (older consoles call it **APIs & Services → OAuth consent screen**).

1. **Branding:** an app name (for example "CreativeCraft") and a support email.
2. **Audience:** choose **External** (unless everyone is in your Google Workspace organisation,
   then Internal is simpler).
3. **Data access:** add the scope `https://www.googleapis.com/auth/drive.file`. That is the only
   scope CreativeCraft needs; don't add broader Drive scopes.
4. **Test users:** while the app is in *Testing*, add the Google account(s) that will sign in.
   Anyone not on the list gets "access blocked".

About **Testing** versus **In production**: in Testing, only test users can sign in, and Google
expires their sign-in after 7 days, so you'd have to sign in again weekly. For anything beyond
trying it out, click **Publish app** under Audience. Because `drive.file` is a non-sensitive
scope, it doesn't need Google's security review.

### 4. Create the OAuth client

1. Go to **Clients** (older consoles: **APIs & Services → Credentials → Create credentials →
   OAuth client ID**).
2. Choose application type **Desktop app**. This matters: CreativeCraft signs in through a
   temporary address on your own computer (`http://127.0.0.1:<port>`), which only Desktop-app
   clients allow. Other types fail with `redirect_uri_mismatch`.
3. Create it, then copy the **Client ID** and **Client secret**. (For desktop apps Google treats
   the secret as not confidential, but there's no reason to post it publicly.)

### 5. Enter it in CreativeCraft

1. Open **Settings → Cloud sync** and choose **Google Drive**.
2. Paste the client ID and secret.

To configure a whole machine or a packaged build instead, set these environment variables before
CreativeCraft starts. They're used whenever cloud sync hasn't been configured yet:

```sh
export CREATIVECRAFT_GOOGLE_CLIENT_ID="1234-abc.apps.googleusercontent.com"
export CREATIVECRAFT_GOOGLE_CLIENT_SECRET="GOCSPX-…"
```

### 6. Sign in

1. In **Settings → Cloud sync**, click **Sign in** (or run `creativecraft-cli login`).
2. Your browser opens Google's sign-in page. Choose the account, and allow access to "files
   created by this app".
3. The browser says "Signed in. You can close this tab and return to CreativeCraft.", and
   Settings shows *Signed in*.
   You have 5 minutes to finish before the sign-in times out.

Then click **Sync now** on the Files page. The first sync uploads your projects folder; on your
other computers, set up the same way and sync to download it.

## Troubleshooting

| What you see | What to do |
|---|---|
| "Access blocked: … has not completed the Google verification process" | You're not a test user. Add the account under Audience → Test users, or publish the app. |
| `redirect_uri_mismatch` | The client isn't a **Desktop app**. Create a new client of that type. |
| `invalid_client` | The ID or secret has a typo or the client was deleted. Paste them again. |
| "Google Drive API has not been used in project … or it is disabled" | Enable the Drive API (step 2) in the same project as the client. It can take a minute to apply. |
| It worked for a week, then sync fails and asks you to sign in | The app is still in Testing, where sign-ins expire after 7 days. Sign in again, or publish the app. |
| The browser never comes back / sign-in times out | Finish within 5 minutes. Firewalls or VPN tools that block local connections to `127.0.0.1` can stop the browser reaching CreativeCraft; allow them for this one sign-in. |
| A file uploaded on drive.google.com doesn't appear locally | Expected: with `drive.file` CreativeCraft only sees files it created. Put the file in the local projects folder instead. |
| Files called `… (cloud copy for …)` | Both computers changed the same file between syncs. Both versions were kept; keep the one you want and delete the other. |

To stop syncing, choose **Off** in Settings → Cloud sync (your files stay where they are). To
disconnect the Google account, click **Sign out**; you can also remove CreativeCraft's access at
<https://myaccount.google.com/permissions>.
