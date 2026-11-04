# Media test fixtures

Generated with ffmpeg (libx264, libvpx, libopus and the native AAC encoder).
Each video is 128x96 at 10 fps: one second of solid red, then one second of
solid blue, with a keyframe every second. The audio is a 2-second 440 Hz sine
at 48 kHz.

```bash
V='color=c=red:s=128x96:r=10:d=1[a];color=c=blue:s=128x96:r=10:d=1[b];[a][b]concat=n=2:v=1:a=0[v]'
A='sine=frequency=440:sample_rate=48000:duration=2'

ffmpeg -filter_complex "$V" -f lavfi -i "$A" -map '[v]' -map 0:a \
  -c:v libx264 -profile:v baseline -pix_fmt yuv420p -g 10 -c:a aac -b:a 32k -ac 2 h264_aac.mp4
ffmpeg -filter_complex "$V" -f lavfi -i "$A" -map '[v]' -map 0:a \
  -c:v libx264 -profile:v high -bf 2 -pix_fmt yuv420p -g 10 -c:a aac -b:a 32k \
  -movflags frag_keyframe+empty_moov+default_base_moof h264_aac_fragmented.mp4
ffmpeg -filter_complex "$V" -f lavfi -i "$A" -map '[v]' -map 0:a \
  -c:v libvpx-vp9 -b:v 50k -g 10 -c:a libopus -b:a 24k vp9_opus.webm
ffmpeg -filter_complex "$V" -f lavfi -i "$A" -map '[v]' -map 0:a \
  -c:v libvpx -b:v 50k -g 10 -c:a libopus -b:a 24k vp8_opus.webm
ffmpeg -f lavfi -i "$A" -c:a aac -b:a 32k aac_only.m4a
```

`h264_ctts_v0_negative.mp4` has no audio and one 2-second keyframe group. It
stores negative B-frame composition offsets in a version 0 `ctts` box, as some
encoders do although the spec makes version 0 unsigned. ffmpeg writes such
offsets only in a version 1 box, so the version byte is then set to 0; the
offsets themselves are unchanged:

```bash
ffmpeg -filter_complex "$V" -map '[v]' -c:v libx264 -profile:v high -bf 2 \
  -pix_fmt yuv420p -g 20 -sc_threshold 0 -movflags +negative_cts_offsets v1.mp4
python3 - <<'EOF'
data = bytearray(open("v1.mp4", "rb").read())
at = data.find(b"ctts")
assert data[at + 4] == 1
data[at + 4] = 0
open("h264_ctts_v0_negative.mp4", "wb").write(data)
EOF
```
