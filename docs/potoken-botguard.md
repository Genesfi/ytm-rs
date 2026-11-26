# Catatan Referensi: BotGuard PO Token On-Demand untuk `ytm-rs`

Dokumen ini merangkum arsitektur playback & cara kerja **BotGuard PO Token** (disarikan dari implementasi Android `PoTokenWebView` / `InnerTubeX` di BitChord) sebagai referensi jika suatu hari `ytm-rs` ingin mendukung PO Token on-demand di PC Windows.

---

## 1. Masalah yang Diselesaikan

1. **Jalur normal (`VisionOS` di `src/innertube.rs`)**:
   - Cepat (<1 detik), gratis Opus (`itag 251`), dan tanpa cookie login.
   - Kekurangan: jika IP/jaringan sedang kena flag bot check YouTube, YouTube menolak dengan `LOGIN_REQUIRED: Sign in to confirm you're not a bot`.
   - Hasil tes langsung dari jaringan saat kena bot flag: client non-login lain seperti `ANDROID_VR` dan `IOS` juga ikut terkena `LOGIN_REQUIRED` (sedangkan `TVHTML5_SIMPLY_EMBEDDED_PLAYER` ditolak `ERROR: YouTube is no longer supported`).

2. **Jalur `WebMusic` (`src/webmusic.rs`, `WEB_REMIX` + `src/jsc.rs`)**:
   - Memakai sesi login + solver `jsc` (`rquickjs` / EJS 0.8.0 fix dari upstream `ab1df31`).
   - Untuk akun **Premium (`true`)**: stream Opus (`itag 774` / `251`) berjalan lancar.
   - Untuk akun **non-Premium (`false`)**: URL awal berhasil didapat, tetapi `googlevideo.com` memutus download di tengah lagu (sekitar `range 851968-...` / ~50 detik) dengan `HTTP 403 Forbidden` karena tidak membawa **BotGuard PO Token**.

3. **Jalur `SafariHls` (`src/ytdlp.rs` / `src/backend.rs`)**:
   - Memakai `yt-dlp` (`--extractor-args youtube:player_client=web_safari`, format `worst[protocol^=m3u8][acodec=mp4a.40.2]`).
   - Bisa memutar lagu meski `VisionOS` kena bot check (selama `yt-dlp.exe` cukup baru, mis. `2026.11.23+`), tetapi butuh ~10–14 detik untuk resolve dan kualitasnya AAC-LC (bukan Opus).
   - Catatan log `WARN mpeg2ts_reader::packet: no payload data present` saat `SafariHls` aktif adalah **normal** (paket padding/adaptation-only di stream MPEG-TS).

---

## 2. Alur BotGuard PO Token (2 Tahap)

BotGuard memeriksa lingkungan DOM/browser penuh (`window`, `document`, `navigator`, `canvas`, `WebGL`, dll.), sehingga **tidak bisa** dijalankan di `rquickjs` polos — harus memakai WebView / Chromium.

### Tahap A — Inisialisasi & Integrity Token (Sekali per beberapa jam)
1. Buat hidden WebView (`withVisible(false)` / offscreen) dengan base URL `https://www.youtube.com` dan User-Agent desktop Chrome.
2. Minta challenge BotGuard ke server Google:
   - `POST https://www.youtube.com/api/jnn/v1/Create`
   - Header:
     - `Accept: application/json`
     - `Content-Type: application/json+protobuf`
     - `x-goog-api-key: AIzaSyDyT5W0Jh49F30Pqqtyfdf7pDLFKLJoAnw`
     - `x-user-agent: grpc-web-javascript/0.1`
   - Body: `["O43z0dpjhgX20SCx4KAo"]`
3. Jalankan `runBotGuard(parsedChallengeData)` di JS WebView:
   - Menghasilkan `webPoSignalOutput` dan `botguardResponse`.
4. Tukar `botguardResponse` menjadi `integrityToken`:
   - `POST https://www.youtube.com/api/jnn/v1/GenerateIT` (header sama seperti di atas)
   - Body: `["O43z0dpjhgX20SCx4KAo", "<botguardResponse>"]`
   - Menghasilkan `integrityToken` dan `expirationTimeInSeconds` (refresh sebelum habis, mis. kurangi margin 10 menit).
5. Panggil `createPoTokenMinter(webPoSignalOutput, integrityToken)` di JS WebView.

### Tahap B — Cetak PO Token per Request (<1 detik saat minter aktif)
1. Encode identifier (misalnya `videoId` atau `visitorData` tergantung target) menjadi `Uint8Array`.
2. Panggil `obtainPoToken(u8Identifier)` di JS WebView.
3. Konversi byte array hasil menjadi string Base64/Base64url — inilah **PO Token**.

---

## 3. Rencana Integrasi On-Demand di `ytm-rs` (Jika Diperlukan Nanti)

Bahan yang **sudah tersedia** di `ytm-rs`:
- `Cargo.toml` sudah memiliki `tao = "0.37.1"` dan `wry = "0.57.0"` (sudah dipakai di `src/login.rs` untuk jendela sign-in).
- `src/webmusic.rs` + `src/jsc.rs` sudah menangani `WEB_REMIX` dan cipher/n-sig solver.

Pola agar tetap **0 MB RAM saat normal**:
1. **Trigger hanya saat bot flag**:
   - Selama `VisionOS` berhasil, WebView2 tidak pernah dinyalakan.
   - Saat `VisionOS` gagal `LOGIN_REQUIRED` (dan akun non-Premium), baru spawn subprocess hidden `wry` WebView2 (mirip pola subprocess login agar terpisah dari event loop `eframe`) untuk menyalakan BotGuard minter & mencetak PO Token.
2. **Teruskan ke `WebMusic`**:
   - Sertakan `serviceIntegrityDimensions: { poToken }` di `player_body()` (`src/webmusic.rs`).
   - Tambahkan `&pot=<po_token>` pada URL stream `googlevideo.com` yang didapat dari `WEB_REMIX`.
3. **Auto-shutdown**:
   - Tutup/kill subprocess hidden WebView2 setelah idle beberapa menit (mis. 5–10 menit) atau setelah `VisionOS` kembali normal.

Alternatif praktis tanpa menulis WebView2 minter sendiri: memasang plugin eksternal `bgutil-ytdlp-pot-provider` agar otomatis dipakai oleh `yt-dlp.exe` yang dipanggil di `src/ytdlp.rs`.
