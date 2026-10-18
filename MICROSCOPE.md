# Microscope reverse-engineering notes

Initial findings from the decompiled Android app. The active UDP route is partly recovered; see [Wire protocol](#wire-protocol) and [Open questions](#open-questions).

## App and device detection

- The package is `com.maxsee.dm_wifi`; `apktool.yml` names the source APK `com.maxsee.dm_wifi_base.apk` and reports minimum SDK 25 (`apktool.yml:1-4`). Vendor camera code is under `com.joyhonest.wifination`.
- The launcher flow checks the phone's Wi-Fi IPv4 address before opening the camera screen: `SplashActivity$1.run()` routes to `MainActivity` when `MyApp.F_IsVaildModel()` succeeds, otherwise to `NoDeviceActivity` (`smali/com/maxsee/dm_wifi/SplashActivity$1.smali:46-107`).
- `getLocalIpAddress()` reads `WifiManager.getConnectionInfo().getIpAddress()`. `int2ip()` formats only the first three octets. With no explicit prefix list, `F_IsVaildModel()` accepts `192.168.25` through `192.168.34`, plus `192.168.123` (`MyApp.smali:1134-1282,2173-2267`). This is a local-address heuristic, not an active probe of a microscope endpoint.
- The binary manifest contains Wi-Fi state/change, network change, internet, external-storage, audio-recording, and wake-lock permissions (`AndroidManifest.xml`; names extracted from its binary XML).

## Live preview path

1. `MainActivity.onResume()` repeats the local-IP check. On success it calls `F_OpenStream(true)`; on failure it closes the stream and posts `DispInvalad` (`MainActivity.smali:2657-2763`).
2. Opening the stream requests a Wi-Fi `Network` and binds the process to it. The callback unbinds for `false`, binds for `true`, then unregisters itself; `F_OpenStream()` explicitly unbinds, waits 180 ms, binds, waits another 180 ms, and calls `wifination.naInit("")` (`MyApp.smali:1991-2051`, `MyApp$1.smali:39-86`, `MainActivity.smali:425-510`).
3. Startup also sets resolution index 5, enables the native reverse-bitmap flag, sets camera data rotation to 90 degrees, and passes the split-display flag to `naSetRota90Disp()`. Closing stops native recording and calls `naStop()` (`MainActivity.smali:425-510`).
4. `naInit()` delegates to native `naInitC()`. `wifination` allocates a direct frame buffer and gives it to JNI; `ReceiveBmp()` copies that buffer into an `ARGB_8888` `Bitmap` and, when no `OnReceiveFrame` handler is installed, posts it on EventBus as `ReceiveBMP` and `onGetFrame`. `MainActivity.ReceiveBMP()` marks the device connected and updates the preview (`wifination.smali:204-212,1921-2095`, `MainActivity.smali:952-1082`).

The Java layer exposes frame handoff; the observed UDP endpoints and established packet fields are documented under [Wire protocol](#wire-protocol), with unresolved bytes called out separately.

## Resolution choices

The resolution dialog checks each choice with `MediaCodecList`: it searches AVC (`video/avc`) encoders and calls `VideoCapabilities.isSizeSupported(width, height)`. Unsupported entries are hidden (`wifination.smali:303-391`, `MainActivity.smali:3226-3356`). Index 5 is the initial choice. Selecting a row updates `MyApp.nResolition`, then applies a pair through native `naSetRecordWH()` (`MainActivity.smali:3016-3610,3613-3714`; `MainActivity$16.smali:88-103` shows a row handler).

| `nResolition` | AVC capability check (width × height) | `naSetRecordWH` arguments |
| ---: | ---: | ---: |
| 1 | 640 × 480 | 480, 640 |
| 2 | 1280 × 720 | 720, 1280 |
| 3 | 1920 × 1080 | 1080, 1920 |
| 4 | 2048 × 1536 | 1536, 2048 |
| 5 (initial) | 2560 × 1440 | 1440, 2560 |
| 6 | 3840 × 2160 | 2160, 3840 |

`naSetRecordWH()` stores a phone-side resize pair; `F_GetPacket()` uses it to prepare recorded frames, and `EncodeSnapshot()` uses it as the still target unless the separate `naSetSnapPhoto(width, height, enabled)` override is enabled. This is a local resize setting; `naSetRecordWH()` sends no camera-resolution command. The native setter may return `-1` when its status condition blocks the change, and the UI ignores that result. The UI's aspect-ratio adjustment remains separate (`MainActivity.F_AdjFrame()`, `MainActivity.smali:204-319`).

## Still capture and recording

- The photo button is wired to `F_CheckPermissions(0)` (`MainActivity$14.smali:46-56`). After permission handling, `F_GetPermissions()` takes a picture only if `MyApp.bIsConnect` is true: it creates a timestamped PNG path (`yyyy_MM_dd_HH_mm_ss_SSS.png`) under `sLocalPath`, plays the photo sound, then calls `wifination.naSnapPhoto(path, 0)` (native type `0`) (`MainActivity.smali:2048-2081`; `MyApp.smali:2054-2132`).
- `F_CreateLocalDir("DMWifi")` initializes `sLocalPath` used to build photo paths: `getExternalFilesDir("DMWifi")` on Android 10+, and `<external-storage>/DMWifi/DMWifi` on older versions. On the normal non-recording photo path, `F_getFd()` uses the path's file name for a MediaStore row rather than writing directly to `sLocalPath` (`MyApp.smali:591-635,637-680`; `MyMediaMuxer.smali:262-466`).
- `naSnapPhoto()` uses `isRecordtoFile`: when true it passes fd `-1`; otherwise `MyMediaMuxer.F_getFd()` inserts a MediaStore image row and returns its descriptor. `F_getFd()` labels `.png` as `image/png`; on Android 10+ it places the row under `DCIM/<sAlam>` (or `DCIM` if unset) (`wifination.smali:3586-3625`; `MyMediaMuxer.smali:262-466`).
- `naSnapPhotoA(path, fd, type)` converts the Java path to UTF-8 and rejects the call with `-1` unless bit 0 of native `nSDStatus` is set. For the app's `type=0`, it calls `m_FFMpegPlayer->SaveSnapshot(path, fd)`: a local snapshot path, not a device capture command (`Java_com_joyhonest_wifination_wifination_naSnapPhotoA` and `C_FFMpegPlayer::SaveSnapshot` in `libJoyWifiCamera.so`).
- `SaveSnapshot` rejects while busy; otherwise it stores the fd/path, sets busy, and launches a detached pthread. It does not check `pthread_create()` failure, which leaves busy set. `F_GetPacket()` keeps the latest decoded `AVFrame` in the player's snapshot cache under a mutex; `EncodeSnapshot()` acquires that mutex after worker scheduling, copies or scales the cached frame, then converts and encodes after unlocking. It samples at worker lock acquisition, not at the button tap; no tap timestamp or frame sequence is retained. A short path or unset cache-ready flag returns `-1`; the thread clears busy on normal return. Encoding is PNG for `.png` and MJPEG otherwise; `bSaveByName != 0` forces MJPEG.
- With fd `-1`, the worker uses `fopen`/`fwrite`/`fclose`; otherwise it duplicates the MediaStore fd, writes, closes the duplicate, and resets the stored fd. Dup/write/close results are unchecked. Path/cache readiness, encoder setup, `avcodec_open2()`, and file-open failures return without the save callback; the `avcodec_open2()` failure path does not explicitly free its codec context. `avcodec_send_frame()`/`avcodec_receive_packet()` failures are not propagated as worker failures, so unchecked encode/write errors may still reach completion as apparent success. The normal success path calls `F_OnSave2ToGallery_midi(0)` → Java `OnSave2ToGallery(path, 0)`, which closes the original `ParcelFileDescriptor`, clears MediaStore `is_pending`, and posts `onSnapPhotoFinish` and `SavePhotoOK` (`wifination.smali:1842-1886`; `MyMediaMuxer.smali:186-259`; native `libJoyWifiCamera.so`). Checked worker failures on the MediaStore path bypass that Java cleanup; `MainActivity` ignores the immediate native return.
- `naSnapPhotoA` has separate nonzero-type branches. Type `1` routes by native `nICType` through the HTTP `snap.cgi` path, `F_SD_Snap()`, or a UDP/20000 `send_cmd_udp()` message (`4a 48 43 4d 44 00 01`) for selected modes. The app's button passes type `0`; the capture's recurring `JHCMD d0 01` payload is not that type-1 message.
- Video recording uses the same timestamp format with `.mp4`, applies the selected `naSetRecordWH()` pair, and starts native recording with `naStartRecord(path, 0)`; stopping uses `naStopRecord_All()` (`MainActivity.smali:2048-2144,513-614`; `MyApp.smali:2054-2132`).

## Wire protocol

No vendor protocol specification is available. This section distinguishes fields observed in `microscope.pcapng`, message construction found in `libJoyWifiCamera.so`, and cross-checks against an independent Max-See implementation. The captured route uses UDP; other native-library strings and branches are not evidence that this microscope session uses them.

### Observed endpoints and socket roles

The monitor-mode capture is 117.2 seconds long and contains 66,446 802.11 frames. Its active IP pair is microscope `192.168.29.1` and phone `192.168.29.101`.

| Traffic | Observed endpoints | Evidence |
| --- | --- | --- |
| JPEG preview | Microscope `192.168.29.1:10850` → phone `192.168.29.101:10900` | 26,105 UDP datagrams in the capture; native type-8 receiver binds local UDP/10900. The source port 10850 is capture evidence, not hard-coded by the receiver. |
| Control | Phone `192.168.29.101` → microscope `192.168.29.1:20000` | 177 captured UDP records: 171 seven-byte `JHCMD D0 01` and six eleven-byte `JHCMD 20 ...`. |
| Startup marker | Phone `192.168.29.101` → microscope `192.168.29.1:20001` | Three eleven-byte `FDWN` prelude records (one 802.11 retry) and one 26-byte time-sync `FDWN` record from phone source port `44734`. |

`libJoyWifiCamera.so` is ELF64 AArch64 (Android 26/NDK r21d), with vendor C++ and JNI symbols retained; it imports libc socket APIs including `socket`, `bind`, `connect`, `sendto`, and `recvfrom` (`readelf -h -d -Ws libJoyWifiCamera.so`). JNI exports include `naSetCameraIPandType`, `naSnapPhotoA`, and `naRemoteSnapshot`; their presence alone does not prove use in this capture.

Native `naInit_Re()` starts generic UDP readers bound locally to `0.0.0.0:20000`, `:20001`, and `:25000` before dispatching by camera type. The type-8 path also binds the JPEG receiver to local UDP/10900. The Java wrapper exposes `naStartRead20000_20001()`, `naWriteport20000()` (up to 200 bytes to the native-state peer), `naStartReadUdp()`, and `naSentUdpData(host, port, bytes, length)`; `onReadPort25000()` forwards received bytes to Java. The capture has no device replies on UDP/20000–20001 and does not establish a role for UDP/25000. A native path named for UDP/10901 appears in symbols, but its role in this session is unverified.

### Type selection and initialization

`MainActivity.F_OpenStream()` calls `naInit("")`. Native `naInitC()` runs `F_GetIP()`, sends the eleven-byte `FDWN` prelude on UDP/20001, then calls `_naInit_()` → `naInit_Re()`. `F_GetIP()` tries `vendor.arc.net.ipv4.host_address`, then `arc.net.ipv4.host_address`, then scans interfaces with `SIOCGIFCONF` and passes local address candidates to `F_AdjIcTypeA()`.

The selector compares the first 12 bytes against `192.168.29.1` and stores `nICType = 8`; it does not require the input string to end there. Thus the captured phone address `.101` matches the selector prefix. The type-8 dispatch entry starts `F_Rec_RTP_Data_Service()`, which binds local UDP/10900 and starts `doReceive_rtp()`. The receiver removes the eight-byte prefix before JPEG handling. This ties the static type-8 path to the captured addresses and traffic, but the pcap does not record which `F_GetIP()` candidate was used or the runtime `nICType` value.

### UDP/20000 control messages

The control magic is five ASCII bytes, `JHCMD` (`4a 48 43 4d 44`), followed by raw command bytes. The byte sequences constructed by the native type-8 branch are:

| Payload after `JHCMD` | Full datagram length | Local evidence |
| --- | ---: | --- |
| `d0 01` | 7 bytes | Constructed natively; 171 captured records, including 10 802.11 retry copies. An independent receiver identifies it as the stream-start/heartbeat command. |
| `20 00 00 00 00 00` | 11 bytes | Constructed natively and seen six times in the capture. The five trailing zero bytes' meaning is unknown. |

The capture contains 161 distinct `JHCMD D0 01` datagrams from `2026-10-16T14:56:48.515743Z` through `2026-10-16T14:58:09.690382Z`, an 81.175-second span. The 160 inter-arrival intervals have a median of 0.505 s and a mean of 0.507 s (about 1.97 Hz); 143 are between 0.5 and 0.6 s. One initial pair is about 13 μs apart, and the longest interval is 1.019 s. This capture supports an approximately 2 Hz client heartbeat during that interval, not a perfectly fixed cadence or a claim about other sessions.

The capture has no UDP/20000 response records, so it does not reveal acknowledgements or command results. The independent [Max-See analysis](https://www.chzsoft.de/site/hardware/reverse-engineering-a-wifi-microscope/) and [receiver implementation](https://github.com/loehnertj/maxsee_viewer/blob/main/src/maxsee.py) cross-check a type-8 path at `192.168.29.*`: that implementation sends seven-byte `JHCMD 10 00`, `JHCMD 20 00`, and `JHCMD D0 01` commands at startup, repeats `D0 01` as a heartbeat, and sends `D0 02` to stop. Its `JHCMD 20 00` datagram is seven bytes, unlike the eleven-byte message emitted by this library. The reference's `10 00` and `D0 02` commands were not observed in this capture; the trailing zero bytes in the local message remain unexplained.

### UDP/20001 `FDWN` and time-sync datagrams

Native `naInitC()` constructs the fixed eleven-byte payload below from a read-only literal plus immediate bytes. No payload byte comes from the JNI method argument; the destination sockaddr is taken from native state.

```text
46 44 57 4e 01 00 01 00 01 00 00
```

After the prelude, `naInit_Re()` calls no-argument `F_naSyncTime()` before dispatching on `nICType`; it constructs a 26-byte `FDWN` time-sync datagram. In `libJoyWifiCamera.so`, `_Z9naInit_Rev` makes this call at `0x18257c`, before its type-dispatch table, and the target `_Z12F_naSyncTimev` is at `0x183420`. Separately, the type-8 setup has a conditional second send of the same fixed eleven-byte prelude. Its guard and destination come from native state, but the payload is not parameter-filled. The capture's three eleven-byte records (one retry) are consistent with these two send sites plus the retry; its distinct 26-byte record matches the time-sync builder:

```text
46 44 57 4e 20 00 01 00 10 00 1a 0a 10 0e 38 2f
02 00 52 91 1a 0a 10 10 38 2f
```

`F_naSyncTime()` takes no arguments. It derives byte `16` from a first time/calendar conversion, then builds the two six-byte calendar tuples from two further conversions. The captured packet (`192.168.29.101:44734` → `192.168.29.1:20001`) has pcap timestamp `2026-10-16T14:56:48.509916Z`. The native 26-byte layout and captured values are:

| Byte offsets | Construction | Captured value |
| --- | --- | --- |
| `0–7` | Fixed literal `FDWN 20 00 01 00` | `46 44 57 4e 20 00 01 00` |
| `8–9` | `10 00` in this shape; consistent with a little-endian count of 16 following bytes | `10 00` |
| `10–15` | Calendar tuple: year minus 100, month plus 1, day, hour, minute, second | `1a 0a 10 0e 38 2f` (`2026-10-16 14:56:47`) |
| `16` | `tm_gmtoff / 3600` from the first time conversion | `02` |
| `17–19` | Fixed bytes; semantics not identified | `00 52 91` |
| `20–25` | Second calendar tuple in the same field order | `1a 0a 10 10 38 2f` (`2026-10-16 16:56:47`) |

This is a time-derived message, not a fixed 26-byte command and not populated from caller-supplied parameters. `naInit_Re()` invokes the builder during initialization; the captured tuple values match that path. The meanings of fixed bytes `00 52 91` and the protocol roles of the two time tuples remain unknown. No UDP/20001 response was captured.

Bytes `8–9` are consistent with a little-endian length for the bytes following that field: the 11-byte prelude has `01 00` and one remaining byte; this 26-byte datagram has `10 00` and 16 remaining bytes. In both cases, the value equals the datagram size minus 10. This strongly suggests a trailing-payload length, not total datagram length, but only these two message sizes are captured, so the protocol meaning is not confirmed.

### UDP/10900 JPEG datagrams

Each received UDP payload begins with an eight-byte header, followed by a JPEG data fragment. One captured packet begins:

```text
01 00 0d 00 08 0f 00 00 ff d8
```

The supported header interpretation is:

| Byte offsets | Interpretation | Confidence |
| --- | --- | --- |
| `0–1` | Little-endian frame counter (`01 00` → frame 1 in the sample) | Decoded by the independent receiver and consistent with capture. |
| `2` | Unknown | Not decoded. |
| `3` | Packet counter within the frame; zero marks a new frame | Decoded by the independent receiver. |
| `4–7` | Unknown | Not decoded. |
| `8…` | JPEG fragment bytes; the sample's first fragment starts with SOI `ff d8` | Confirmed by the capture and native eight-byte stripping path. |

The independent receiver appends bytes from offset 8 for each frame and treats skipped or out-of-order packet counters as a corrupt frame to drop. This is a demonstrated reassembly policy for that receiver; the remaining six header bytes and any device-side recovery behavior are unknown. SOF parsing of this capture found 1280 × 720 JPEGs except for one malformed candidate. The phone app's normal photo button uses the local `type=0` snapshot path, so the lack of a per-photo control datagram is expected and does not define another wire command.

### Other native network strings

Other candidate endpoints in the native library include `rtsp://192.168.27.1:8080/?action=stream`, `rtsp://192.168.26.1`, and `rtsp://192.168.25.1`, plus HTTP URLs for `.25.1` and `.26.1`. IP literals span `192.168.25` through `.35`, with additional candidates `192.168.123`, `.234`, `.254`, and `175.16.10`.

A separate-looking HTTP path targets `192.168.234.1:80`: `/web/cgi-bin/hi3510/snap.cgi?&-getpic&-chn=0` and `/web/cgi-bin/hi3510/switchrec.cgi?-chn=11`. Other unplaced strings include `AMEBA:DISCOVER:`, `GPSOCKET`, `UDPSOCKE`, `JHCMD0`, `JHCMDTD`, `GETMODE`, `BROWSE;img;%003d`, `BROWSE;vid;%003d`, `BROWSE;pir;%003d`, and `DOWN;%s`. Their wire context and relation to this microscope capture are not established; do not treat them as active type-8 traffic.

## Open questions

- The selector statically maps a local address beginning `192.168.29.1` to `nICType = 8`, and the captured phone address matches; the pcap does not record which `F_GetIP()` candidate or runtime type was selected. `JHCMD 20 ...` and the meanings of the 11-byte `FDWN` fields remain unknown. Bytes `8–9` likely encode the little-endian number of bytes following that field (`1` in the prelude, `16` in the time-sync packet); only those two captured sizes support the inference. The 26-byte `FDWN` is generated by `F_naSyncTime()` from phone time; the meanings of its fixed `00 52 91` bytes and the protocol roles of its two time tuples remain unknown. No camera responses appear on UDP/20000–20001, so acknowledgement semantics remain unknown.
- The worker snapshots the cache when its asynchronous thread acquires the player mutex, not at tap time. The exact cache age then remains unknown; validating tap-to-frame timing requires tap timestamps and saved photos.
- What dimensions did still photos actually use after a mode change? The pcap's 1280 × 720 SOFs are incoming preview frames, not saved photos. The app ignores the setter result and does not call `naSetSnapPhoto()`, so runtime setter success and saved-photo dimensions remain unverified. Since `naSetRecordWH()` sends no camera-resolution command, this app path does not test whether the microscope accepts another mode.

## Rust library and GTK4 example

The root Cargo package exposes a reusable `microscope` library and a GTK4 live-view example. Connect the PC to the microscope Wi-Fi network; the default device address is `192.168.29.1`.

```sh
cargo run --example gtk_live_view --features gtk-example
```

The example accepts a microscope IPv4 address, displays the latest reassembled JPEG, and saves captures as timestamped `.jpg` files in the current directory. GTK4 development files and `pkg-config` are required to build the example. The library itself does not enable or depend on GTK by default.

Applications can use the nonblocking library API directly:

```rust
use microscope::{Microscope, MicroscopeConfig};
use std::{net::Ipv4Addr, thread, time::Duration};

fn main() -> std::io::Result<()> {
    let mut camera = Microscope::connect(MicroscopeConfig::new(Ipv4Addr::new(192, 168, 29, 1)))?;
    loop {
        if let Some(frame) = camera.poll()? {
            frame.save_jpeg("capture.jpg")?;
        }
        thread::sleep(Duration::from_millis(15));
    }
}
```

`Microscope::connect()` binds UDP/10900 by default, sends one fixed UDP/20001 `FDWN` prelude and a locally generated time-sync datagram, then sends `JHCMD D0 01` immediately and approximately every 500 ms. `Microscope::poll()` drains pending datagrams and returns the newest complete frame; call it regularly to service the heartbeat. The source IP must match the configured device IP, but the source UDP port is not restricted.

Frames are assembled from the eight-byte header's fragment counter. A missing, out-of-order, or changed-frame fragment invalidates the partial frame; a complete JPEG is returned after the next fragment-counter-zero packet marks the boundary. The implementation drops frames larger than 16 MiB and JPEG candidates without SOI/EOI markers. The GTK example does not expose resolution selection: the documented `naSetRecordWH()` value is phone-side resizing and is not a camera-resolution command. The eleven-byte `JHCMD 20 ...` message is not sent because its trailing bytes' meaning and sequence are unknown.
