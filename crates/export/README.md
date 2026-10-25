# EffectCraft export

Render Queue output modules, frame processing and movie/image/audio export in pure Rust.
Codec implementations are accessed through the workspace's FilmCraft and EffectCraft crates.

## Uncompressed QuickTime intermediates

- `uncompressed`: existing 8-bit RGB/ARGB (`raw `), including odd-width row padding.
- `uncompressed16`: full-range 16-bit ARGB (`b64a`), big-endian samples in A, R, G, B order,
  with PCM audio. Select **Uncompressed 16-bpc MOV** in the Render Queue or pass
  `{"format":"uncompressed16"}` to `renderQueue.add` / `renderQueue.setOutputModule`.

The 16-bpc path converts rendered float pixels directly, without an 8-bit intermediate.
RGB composites over the composition background with opaque alpha; RGB + Alpha respects the
Straight/Premultiplied setting; Alpha writes the matte into RGB with opaque alpha. Values are
rounded to 0–65535 and clipped to that range. Render Settings and project color depth still
apply: choose a 16/32-bpc project for source precision beyond 8 bits. This is integer output,
so it does not preserve HDR values outside 0–1.

The renderer supplies the project's output-space values. As with the existing raw MOV output,
no color-profile or alpha-association metadata is embedded; interpret those settings explicitly
in the receiving application. RGB also occupies 64 bits per pixel. At 1080p30 the video alone
is about 498 MB/s. Frames are limited to 256 MiB (including 7680×4320 at 16 bpc); one full-precision
frame is processed at a time to bound export working memory.

In-memory outputs (including browser downloads) are limited to 512 MiB and grow through
fallible allocations; oversized output returns an error. Native filesystem output streams
to disk without this file-size limit.

## Specifications used

- Apple **Technical Q&A QA1114**, edition **2002-02-07**,
  [48 bit & 64 bit Pixel Format support in QuickTime](https://developer.apple.com/library/archive/qa/qa2001/qa1114.html):
  `b64a`, component order, unsigned full-range 16-bit big-endian samples.
- Apple **QuickTime File Format**, online reference accessed **2026-10-25**,
  [Video sample description](https://developer.apple.com/documentation/quicktime-file-format/video_sample_description)
  and [Video sample data](https://developer.apple.com/documentation/quicktime-file-format/video_sample_data):
  visual sample descriptions and packed uncompressed RGB samples.

No third-party codec implementation was used as a source. Tests inspect stored sample values
and, when installed, invoke ffmpeg/ffprobe as external decode and metadata oracles. They cover
precision, odd dimensions, frame timing, channel modes and PCM audio. The external decode check
does not establish compatibility with every finishing application.
