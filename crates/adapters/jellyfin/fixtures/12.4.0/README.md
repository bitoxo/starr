# Jellyfin 12.4.0 fixture

`system_info.json` is derived from the 12.3.0 fixture with only `Version`
changed. Jellyfin 12.4 only updated the .NET runtime
([release notes](https://github.com/jellyfin/jellyfin/releases/tag/v12.4):
"Update to .net 10.0.14"), with no API changes, so the response shape is
unchanged. The version string `12.4.0` and `ProductName` `Jellyfin Server`
were confirmed on a real 12.4.0 server.
