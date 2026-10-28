# Enhanced Outlook

A portable Windows desktop mail client for Outlook on the web (Exchange 2016/2019 OWA), with optional two-way sync of an HCL Domino web mail account. The user interface is Korean only.

No support is provided for this program. It was built for a specific purpose, so it is not recommended unless you really need it.

## Features

- Outlook on the web: NTLM or forms login, folder and item sync, read, flag, move, delete, purge, drafts saved to the server
- Offline reading and search: SQLite and EML store, full-text index (tantivy) with Korean bigram tokenization
- Compose: rich text, tables, inline images, attachments, signature, templates, address autocomplete, editable quoted text
- HCL Domino web mail (optional): two-way sync, unified inbox
- Import EML, ZIP, HTML backups, PST, HCL Traveler `.ims`; export to EML or ZIP
- Tools: server-only delete, purge, move/copy by date range, mailbox cleanup, duplicate mail
- Portable: all data in an `EnhancedOutlook` folder next to the executable

## Requirements

- Windows 10/11 with the WebView2 runtime
- To build: Rust (stable, MSVC), Visual Studio 2022 Build Tools, Node.js LTS

## Build

```powershell
cd app; npm install; cd ..
.\scripts\package.ps1 -Build
```

Output: `target\dist\EnhancedOutlook-<version>\EnhancedOutlook.exe`

## License

[AGPL-3.0](LICENSE)

Created by [itinfra7 from GitHub](https://github.com/itinfra7).
