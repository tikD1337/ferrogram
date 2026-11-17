# TDLib partial-file layout probe

Run from the repository root, offline:

```powershell
python spikes/partial-file-probe/probe.py
```

This checks the pinned local TDLib downloader's `pwrite(slice, part.offset)` call
and exercises a sparse file at a nonzero physical offset. The MP4 atom list also
checks that the demo container has `moov` before `mdat`. The synthetic probe is
**not** evidence of a live Telegram download. It never opens account data.

To additionally compare an explicitly supplied partial file with its original:

```powershell
python spikes/partial-file-probe/probe.py --partial path/to/partial --source path/to/original --offset 2097152
```

The supplied file is read only; the synthetic file is written under
`target/milestone6/`. The default TDLib source is the machine's existing
`E:/tdlib/td/td/telegram/files/FileDownloader.cpp`, pinned in the milestone report.
The product reads TDLib's reported path directly and uses no `readFilePart` RPC.
