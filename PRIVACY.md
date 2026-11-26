# Kilat Privacy Policy

Kilat is a desktop database client that runs on your computer. This page
explains what data Kilat handles, and what it does when you choose to sign in
with Google for Settings Sync.

## What stays on your computer

Connection settings, saved queries, query history, logs and preferences are
stored in Kilat's data folder on your computer. Saved database passwords are
stored in your operating system's credential store (macOS Keychain, Windows
Credential Manager or the Linux Secret Service). Kilat has no server of its own
and does not collect analytics or telemetry. The author of Kilat does not
receive your data.

Kilat contacts GitHub (github.com) only to check for and download app and
extension updates.

## Google Drive sync (optional)

Settings Sync with Google Drive is off until you click "Connect Google Drive".

- **Access requested:** the `drive.file` scope only. Kilat can see and change
  only the files it created itself, in a folder named "Kilat Sync" in your
  Drive. It cannot read your other files.
- **What is uploaded:** one file, `kilat-sync.json`, with the settings you
  chose to sync (connections without passwords, folders, favorite queries,
  appearance, developer tools, grid preferences, a list of installed
  extensions). Saved passwords are never uploaded unless you turn on
  encrypted password sync; they are then encrypted on your computer with a
  passphrase that only you know, before upload.
- **Where it goes:** straight from your computer to Google Drive. It is not
  sent to the author of Kilat or to any other third party.
- **Your email address:** Kilat reads the email address of the signed-in
  account only to show it on the Sync page.
- **Sign-in tokens:** the refresh token is stored in your operating system's
  credential store and used only to talk to Google Drive.

## Stopping and deleting

- In Kilat, open Settings > Sync and choose "Turn off sync". This removes the
  sign-in from your computer and the local copy of the sync file.
- To revoke access completely, remove Kilat in your Google Account under
  Security > Third-party apps with account access.
- To delete the synced data, delete the "Kilat Sync" folder in Google Drive.

## Use of Google user data

Kilat's use and transfer of information received from Google APIs adheres to
the [Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
including the Limited Use requirements. Data from Google Drive is used only to
provide Settings Sync to the user and is not used for advertising, sold, or
shared with others.

## Contact and changes

Questions or requests: open an issue at <https://github.com/abu-abbas/kilat/issues>.
Changes to this policy are published in this file, which is versioned in the
repository.
