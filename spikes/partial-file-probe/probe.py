"""Offline offset-layout probe; never opens an account or private TDLib cache.
The pinned TDLib downloader's actual pwrite call is checked separately from the
synthetic file exercise. An optional supplied partial file can be compared with
an expected source at the caller's explicitly supplied offset.
"""
from pathlib import Path
import argparse, struct
p=argparse.ArgumentParser()
p.add_argument('--partial',type=Path)
p.add_argument('--offset',type=int,default=2*1024*1024)
p.add_argument('--source',type=Path,default=Path('crates/app/assets/demo-video.mp4'))
a=p.parse_args()
source=Path('E:/tdlib/td/td/telegram/files/FileDownloader.cpp').read_text(encoding='utf-8')
assert 'fd_.pwrite(slice, part.offset)' in source
synthetic=a.partial is None
raw=a.source.read_bytes()
expected=raw[:128*1024] if synthetic else raw[a.offset:a.offset+128*1024]
assert expected, "No source bytes at the requested offset"
partial=a.partial or Path('target/milestone6/partial-layout.bin')
if synthetic:
    with partial.open('wb') as f:
        f.seek(a.offset);f.write(expected);f.flush()
with partial.open('rb') as f:
    f.seek(a.offset);actual=f.read(len(expected))
assert actual==expected
print('TDLIB_SOURCE=absolute-offset-pwrite CONFIRMED')
print('PARTIAL_FILE_PROBE=' + ('SYNTHETIC_PASS live-download-not-tested' if synthetic else 'SUPPLIED_FILE_PASS'))
raw=a.source.read_bytes();pos=0;atoms=[]
while pos+8<=len(raw):
    size,kind=struct.unpack_from('>I4s',raw,pos)
    if size<8:break
    atoms.append((kind.decode('ascii','replace'),pos,size));pos+=size
print('MP4_ATOMS='+repr(atoms))
