# Lyre

> **English:** [README.md](README.md)

![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20Windows-lightgrey)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange)

**Lyre adalah pemutar musik TUI mode tamu untuk Spotify + YouTube Music. Tanpa akun, tanpa login, tanpa wizard penyiapan — tinggal cari dan putar.**

Satu pemutar terminal untuk kedua katalog: pencarian tamu terpadu dengan
filter jenis dan platform, musik yang sedang tren di layar utama, playlist
dari tautan bagikan, antrean campuran dengan autoplay bawaan, lirik
tersinkron, equalizer 10-band, Clean Playback, dan visualizer audio live —
semuanya ramah keyboard dan mouse, dalam satu binary Rust.

## Tentang

Lyre adalah pemutar musik terminal (TUI) yang ditulis dengan Rust memakai
[Ratatui](https://ratatui.rs) + crossterm. Ia memperlakukan Spotify dan
YouTube Music sebagai dua katalog dalam satu perpustakaan. Kamu mencari di
keduanya sekaligus (atau memfilter ke salah satu), membuka album dan
playlist publik, mengantrekan lagu dari sumber mana pun dalam urutan apa
pun, dan Lyre mengganti backend secara otomatis saat antrean berpindah
sumber.

Cara putarnya begini: lagu YouTube Music dialirkan langsung — `yt-dlp`
mengurai stream audionya dan `mpv` memutarnya. Lagu Spotify dicocokkan
lewat metadata: Lyre mengambil judul dan artis dari katalog Spotify lalu
mencari rekaman yang sama di YouTube Music (model yang dipopulerkan
Spotube), kemudian memutar audio itu lewat jalur yt-dlp + mpv yang sama.
Bilah Now Playing mengatakannya dengan jujur: `Spotify - audio via YT
Music`. Karena audionya tidak pernah datang dari server Spotify, **tidak
ada syarat Premium dan tidak ada syarat akun di mana pun di Lyre** —
semuanya berjalan dalam mode tamu.

## Filosofi

- **Diambil dari nama lira Apollo.** Dalam mitologi Yunani, lira adalah
  alat musik Apollo, dewa musik dan puisi — alat yang kecil dan presisi
  tetapi gaungnya jauh. Lyre si aplikasi mengincar proporsi yang sama:
  satu alat yang fokus, tanpa bloat, yang menaruh dua katalog musik
  terbesar dunia di ujung jarimu.
- **Musikmu, tanpa akun dan tanpa taman berpagar.** Lyre memang sengaja
  tidak punya login. Playback tidak pernah membutuhkannya, jadi tidak ada
  yang perlu didaftarkan, tidak ada kunci developer yang harus dibuat,
  tidak ada token milikmu yang disimpan. Penemuan menggantikan pustaka
  akun: pencarian menemukan playlist publik, dan tautan bagikan yang
  ditempel menyematkan yang penting di sidebar-mu.
- **Data jujur, selalu berlabel.** Setiap lagu membawa sumbernya —
  `[SPOT]` atau `[YTM]` — dan setiap fallback juga berlabel: audio
  Spotify diumumkan sebagai "audio via YT Music", daftar perdana luring
  berlabel EXAMPLE TRACKS, dan sumber yang kena rate limit menampilkan
  jam retry yang sebenarnya, bukan diam-diam mengarang hasil. Kalau
  sebuah sumber gagal, UI menyebut langkah mana yang gagal; Lyre tidak
  pernah memalsukan data.
- **Kerajinan terminal.** Antarmukanya dibangun keyboard-first (dengan
  dukungan mouse penuh bila kamu mau), digambar dengan hemat: tanpa
  emoji, wordmark teks serif italic, satu palet aksen emas, dan footer
  yang selalu memberi tahu tombol apa yang berlaku saat itu.
- **Local-first.** Pengaturan, cache, dan log tinggal dalam satu
  direktori konfigurasi di mesinmu. Satu-satunya token yang pernah
  dipegang Lyre adalah token web-player *anonim* Spotify, hanya di
  memori — yang menyentuh disk hanyalah masa berlakunya, tidak pernah
  tokennya sendiri.

## Fitur

- **Pencarian tamu terpadu** di Spotify + YouTube Music — lagu, album, dan
  playlist publik dalam satu daftar, dengan **chip jenis** (All / Songs /
  Albums / Playlists, tombol `[` `]`) dan **filter platform** (All /
  Spotify / YT Music, tombol `f`) yang benar-benar mengarahkan
  pengambilan data: sumber yang dinonaktifkan tidak pernah dipanggil.
- **Layar utama trending** — tangga lagu gabungan yang diselang-seling
  dari YouTube Music (charts Innertube tamu) dan playlist publik Top 50
  Global Spotify, tersimpan di cache disk lengkap dengan jam ambilnya.
- **Playlist dari tautan bagikan** — tempel tautan playlist Spotify atau
  YouTube Music (atau album Spotify) dan ia terbuka dengan nama aslinya
  serta tetap **tersemat di sidebar**.
- **Antrean campuran dengan autoplay** — antrekan lagu dari kedua sumber
  dalam urutan apa pun; dengan Autoplay menyala (bawaan, tombol `A`)
  antrean mengisi sendiri dengan pilihan radio YouTube Music berdasarkan
  lagu yang sedang diputar, berlabel `· autoplay`. Pilihanmu sendiri
  selalu menyisip di depan blok autoplay.
- **Lirik tersinkron** dari LRCLib (tombol `L`) yang menggantikan kotak
  trending di tempatnya, mengikuti lagu yang diputar, dan menerima offset
  waktu ± (tombol `+` / `-`).
- **Equalizer 10-band** — 31 Hz–16 kHz + preamp, −12…+12 dB, 12 preset
  (Flat, Bass Boost, Treble Boost, Vocal, Pop, Rock, Jazz, Classical,
  Electronic, Acoustic, Lo-Fi, Podcast) plus slot simpan Custom 1/2/3,
  diterapkan ke playback sebagai rantai filter `lavfi` mpv yang nyata.
  EQ ringkas + volume juga tinggal di sidebar.
- **Clean Playback** — di YouTube Music, stream datang langsung dari
  yt-dlp dan SponsorBlock memotong segmen sponsor / non-musik; di
  Spotify, item antrean bertipe iklan dibisukan dan dilewati.
- **Lima template visualizer** via [cava](https://github.com/karlstav/cava)
  — Bars, Mirror, Wave, Line (braille), dan LED — diganti dengan `V` atau
  klik, dihaluskan dua lapis agar gerakannya terasa analog, tidak patah.
- **Dukungan mouse di mana-mana** — klik untuk memilih, klik ganda untuk
  memutar/membuka, klik bilah progres untuk seek, seret baris EQ dan
  volume; formulir bersifat modal dan `Esc` selalu bisa mengeluarkanmu.
- **UI full bahasa Inggris**, splash title card, dan diagnostik bawaan:
  `lyre doctor` dan `lyre resolve-test`.

## Screenshot

Dirender dari proses nyata binary v0.1.7 melawan backend stub lokal
(data fixture katalog publik; tanpa data pribadi).

**Splash** — wordmark serif italic sebagai title card terpusat:

![Splash](docs/screenshots/splash.png)

**Home** — trending dari kedua sumber, visualizer Line (braille) dan EQ
sidebar di bawahnya:

![Home](docs/screenshots/home.png)

**Search** — satu kueri, dua katalog: lagu, album, dan playlist dengan
badge `[SPOT]` / `[YTM]` serta filter jenis + platform:

![Search](docs/screenshots/search.png)

**Queue** — antrean campuran dua sumber; pilihan autoplay berlabel dan
header-nya menandai dari mana mereka mulai:

![Queue](docs/screenshots/queue.png)

**Equalizer** — preset Bass Boost di 10 band + preamp, lengkap dengan
rantai filter mpv nyata yang dihasilkannya:

![Equalizer](docs/screenshots/equalizer.png)

**Visualizer** — template Wave (siluet terisi), satu dari lima, dari dekat:

![Visualizer — template Wave](docs/screenshots/visualizer.png)

## Instalasi

Lyre membutuhkan tiga alat runtime — **mpv**, **yt-dlp**, dan **cava** —
plus toolchain Rust untuk build. Script yang disertakan memasang alat
runtime untukmu di Fedora, Debian/Ubuntu, dan Arch:

```bash
./scripts/install-deps.sh
```

…atau tekan `i` di dalam aplikasi dan Lyre menjalankan instalasi yang
sama di terminal aslimu. Perintah manualnya, per distro:

### Fedora

```bash
sudo dnf install mpv yt-dlp cava
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust, bila belum ada
```

### Debian / Ubuntu

```bash
sudo apt install mpv yt-dlp cava
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust, bila belum ada
```

### Arch

```bash
sudo pacman -S mpv yt-dlp cava
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust, bila belum ada
```

### Build dari source

```bash
git clone https://github.com/FathirThoriq/Lyre.git
cd Lyre
cargo build --release
./target/release/lyre           # jalankan TUI
./target/release/lyre doctor    # checklist dependensi + sumber tamu
```

### Launcher dan inisialisasi shell (`lyr`)

Untuk instalasi permanen setingkat user (tanpa sudo), jalankan:

```bash
./scripts/install-launcher.sh
```

Script itu menyalin binary ke `~/.local/bin/lyre`, membuat symlink
`~/.local/bin/lyr`, dan — untuk setiap shell yang ada di mesinmu —
menambahkan **satu** blok bertanda yang idempoten ke file rc-nya.
Menjalankannya lagi mengganti blok itu; tidak pernah menggandakan. Ini
persis yang ditulis script tersebut:

**bash** — `~/.bashrc` (dan sama untuk **zsh** — `~/.zshrc`):

```bash
# >>> lyre >>>
export PATH="$HOME/.local/bin:$PATH"
alias lyr='lyre'
# <<< lyre <<<
```

**fish** — `~/.config/fish/config.fish`:

```fish
# >>> lyre >>>
set -gx PATH $HOME/.local/bin $PATH
alias lyr lyre
# <<< lyre <<<
```

Mau manual? Tambahkan `~/.local/bin` ke PATH dengan sintaks shell-mu
sendiri (blok di atas) dan panggil binary-nya `lyre` — jalan pintas `lyr`
hanyalah alias plus symlink.

Hapus semua yang dipasang launcher (blok + kedua binary) dengan:

```bash
./scripts/install-launcher.sh --uninstall
```

Lalu buka terminal baru dan jalankan `lyre` — atau cukup `lyr`.

### Windows

Tidak ada `lyre.exe` prebuilt yang diterbitkan — catatan jujur:
binary-nya dibangun dan terverifikasi di Linux, dan build Windows harus
dibuat di mesin Windows asli. Kodenya sendiri lintas platform (Ratatui +
crossterm, path via crate `dirs`, tanpa API khusus Unix). Di Windows
10/11 dengan Windows Terminal:

```powershell
# 1. Pasang Rust dari https://rustup.rs (toolchain MSVC), lalu:
# 2. Dependensi runtime (atau jalankan .\scripts\install-deps.ps1):
winget install yt-dlp.yt-dlp
winget install mpv-player.mpv

# 3. Build dan jalankan:
cargo build --release
.\target\release\lyre.exe
```

Satu catatan Windows: visualizer ditenagai **cava, alat khusus
Linux/macOS**. Di Windows kotak visualizer menampilkan petunjuk
"unavailable"-nya dan semua hal lain — pencarian, playback, EQ, lirik,
antrean — bekerja tanpa perubahan. Pengaturan tinggal di
`%APPDATA%\Lyre\` (lihat di bawah).

## Cara Penggunaan

Jalankan dengan `lyre` (atau `lyr` setelah instalasi launcher). Tombol
apa pun keluar dari splash. Lyre punya **lima layar**, selalu terlihat di
sidebar:

1. **Now Playing** — kotak trending (atau lirik) + kartu now playing
2. **Search** — satu pencarian di kedua katalog
3. **Queue** — antrean campuran dan blok autoplay
4. **Equalizer** — 10 band + preamp, preset, slot custom
5. **Clean Playback** — saklar iklan/sponsor

`Tab` memutar fokus panel (Navigation → List → EQ Sidebar → bilah Now
Playing); footer selalu menyatakan fokus saat ini beserta tombolnya.

### Keybindings

| Tombol | Aksi |
| --- | --- |
| `1`–`5` | Ganti layar (Now Playing, Search, Queue, Equalizer, Clean Playback) |
| `Tab` / `Shift+Tab` | Putar fokus panel (footer menampilkan fokus + tombolnya) |
| `q` | Keluar |
| `Space` | Putar / jeda (di Clean Playback dengan fokus List: ganti saklar) |
| `Enter` | Putar lagu terpilih / buka album atau playlist terpilih |
| `n` / `p` | Lagu berikutnya / sebelumnya |
| `/` | Ke Search dan fokuskan input |
| `[` / `]` | Search: putar chip jenis (All / Songs / Albums / Playlists) |
| `f` | Search: putar filter platform (All / Spotify / YT Music), menjalankan ulang pencarian |
| `a` | Tambah ke antrean (menyisip sebelum blok autoplay) |
| `A` | Nyalakan/matikan Autoplay |
| `L` | Nyalakan/matikan lirik tersinkron di kotak trending |
| `+` / `-` | Offset lirik ±0,5 dtk saat lirik tampil, selain itu volume |
| `,` / `.` | Offset lirik −/+ di tempat lain; preset EQ sebelumnya / berikutnya di layar Equalizer |
| `v` | Nyalakan/matikan visualizer sidebar |
| `V` | Putar template visualizer (Bars / Mirror / Wave / Line / LED) |
| `e` | Ke Equalizer (alias dari `4`) |
| `s` | Shuffle (Now Playing / Queue) / simpan preset EQ (Equalizer) |
| `r` | Putar mode repeat (Off / All / One) |
| `0` | Reset EQ ke Flat (Equalizer) |
| `d` | Hapus item antrean terpilih (Queue) |
| `c` | Bersihkan antrean (Queue) |
| `J` / `K` | Pindahkan item antrean terpilih ke bawah / atas (Queue) |
| `PgUp` / `PgDn` | Gain EQ ±3 dB (Equalizer) |
| `x` | Hapus playlist tersemat di bawah penanda (fokus Navigation) |
| `i` | Pasang dependensi yang kurang (mpv / yt-dlp / cava) di terminal asli |
| `Ctrl+V` | Tempel URL utuh ke formulir tempel tautan (tempel bracketed bekerja di mana saja — menempel tautan membuka formulir terisi) |
| `Esc` | Tutup formulir → keluar dari playlist/album yang dibuka → fokus kembali ke Navigation. Selalu bisa mundur |
| Panah | Bertindak pada panel yang fokus. Di daftar: pindah pilihan. Di EQ sidebar: pilih baris / sesuaikan. Hampir di semua tempat lain: `←` / `→` seek −5 dtk / +5 dtk |
| Mouse | Klik item nav, playlist sidebar dan entri tempel tautan, baris EQ (seret untuk mengatur), baris lagu (klik ganda = putar), bilah progres (klik = seek), chip pencarian. Ditangguhkan selama formulir modal terbuka |

### Alur sehari-hari

- **Cari dan saring.** Tekan `/`, ketik, `Enter`. Persempit dengan chip
  jenis (`[` `]`) atau batasi ke satu platform dengan `f` — mengubah
  salah satunya menjalankan ulang pencarian, dan platform yang
  dinonaktifkan tidak pernah dihubungi.
- **Playlist dari tautan bagikan.** Klik (atau navigasikan ke)
  **[ + Paste Playlist Link ]** di sidebar dan tempel URL playlist/album
  Spotify (`open.spotify.com/playlist/…`, `/album/…`) atau URL playlist
  YouTube Music (`music.youtube.com/playlist?list=…`). Ia terbuka dengan
  nama aslinya dan tetap tersemat di sidebar; `x` menghapus entri
  tersemat. (URL pendek `spotify.link` ditolak dengan petunjuk — Lyre
  tidak mengikuti shortener.)
- **Autoplay.** Menyala secara bawaan. Saat tersisa kurang dari 3 lagu
  setelah yang sedang diputar, Lyre menambahkan hingga 10 pilihan radio
  dari lagu yang diputar, dideduplikasi terhadap antrean dan riwayat
  sesi ini. Pilihan berlabel `· autoplay`; pilihan manualmu selalu
  menyalip ke depannya, dan memulai lagu lain dengan tangan mengisi
  ulang radio dari lagu baru itu.
- **Pengaturan dan log.** Semuanya tinggal di direktori konfigurasi —
  Linux: `~/.config/lyre/`, Windows: `%APPDATA%\Lyre\`:
  - `settings.json` — EQ, volume, saklar Clean Playback, toggle autoplay
    dan visualizer + template, dan `saved_playlists` tersematmu
  - `lyre.log` — peristiwa playback dan rincian error (token tidak pernah
    dicatat)
  - `trending_cache.json`, `spotify_trending_cache.json` — cache trending
  - `spotify_state.json` — cooldown rate limit Spotify + masa berlaku
    token anonim (tidak pernah tokennya sendiri)
  - `cava.conf` — konfigurasi visualizer yang digenerate, ditulis ulang
    saat spawn
- **Diagnostik.** `lyre doctor` mencetak checklist (mpv / yt-dlp / cava
  beserta versinya, direktori konfigurasi, langkah tamu Spotify mana
  yang bekerja, keterjangkauan Innertube dan LRCLib, cooldown rate limit
  yang aktif beserta jam retrynya). `lyre resolve-test "Tulus -
  Monokrom"` menjalankan satu lagu lewat jalur resolve yt-dlp yang
  sebenarnya dan mencetak judul hasil pencocokan + host stream-nya.

## Cara Kerja

- **Data YouTube Music** — panggilan tamu ke API **Innertube** YouTube
  (`music.youtube.com/youtubei/v1`, konteks WEB_REMIX, bentuk request
  ytmusicapi) **tanpa header Authorization sama sekali**: pencarian,
  charts, browse album/playlist, dan radio `next` di balik autoplay.
- **Data Spotify** — pembacaan katalog (pencarian, album, playlist
  publik, Top 50) memakai **token web-player anonim** Spotify sendiri:
  diambil dari endpoint token embed publik, dengan fallback mengekstrak
  token yang tertanam di halaman embed publik — mekanisme yang sama
  dengan yang dipakai web player resmi untuk browsing tanpa login.
  Tanpa Client ID, tanpa secret.
- **Disiplin rate limit Spotify** — token anonim punya kuota nyata.
  Semua panggilan API Spotify lewat satu choke point (jarak ≥ 1 detik);
  HTTP 429 memulai cooldown global persisten yang menghormati
  `Retry-After`, selama itu Lyre **tidak** memanggil Spotify sama sekali
  dan menampilkan `Spotify rate limited — retry at HH:MM:SS`. Volume
  panggilan dijaga rendah: satu request pencarian gabungan, trending
  dari cache disk 6 jam, browse satu halaman, cache pencarian per sesi.
- **Audio** — `yt-dlp` mengurai stream (langsung untuk lagu YouTube
  Music; lewat pencocokan "Artis - Judul" untuk lagu Spotify), `mpv`
  memutarnya, dan posisi / jeda / akhir lagu datang dari **socket IPC
  mpv** — itulah yang membuat antrean maju sendiri dan lirik tetap
  sinkron.
- **Lirik** — lirik tersinkron dari [LRCLib](https://lrclib.net),
  terikat pada identitas lagu yang diputar sehingga pergantian lagu
  tidak pernah menampilkan baris basi.
- **Visualizer** — Lyre men-spawn **cava** dengan konfigurasi output raw
  yang digenerate dan merender frame aslinya (output audio sistem — dalam
  praktiknya, mpv). Tidak ada cava, tidak ada gerakan palsu: kotaknya
  menampilkan petunjuk sebagai gantinya.

## Pemecahan Masalah

- **"Spotify rate limited — retry at HH:MM:SS"** — kuota anonim Spotify
  untuk koneksimu habis; catatan itu adalah `Retry-After` sebenarnya
  dari respons 429 Spotify. Lyre berhenti memanggil Spotify sampai jam
  itu (bahkan lintas restart) dan semua hal lain — YouTube Music,
  playback, antrean — terus bekerja. Mencari lagi setelah jam retry
  hanya menghabiskan tepat satu panggilan probe.
- **Visualizer bilang cava hilang / unavailable** — pasang dengan
  `./scripts/install-deps.sh` atau tekan `i` di aplikasi. Di Windows ini
  memang diharapkan (cava hanya Linux/macOS); Lyre lainnya tidak
  terpengaruh.
- **YouTube menjawab "Sign in to confirm you're not a bot"** — itu bot
  check YouTube pada langkah resolve stream, biasanya terpicu dari IP
  datacenter/VPN. Koneksi rumah tangga normalnya lolos. Lyre menampilkan
  pesan yt-dlp itu apa adanya di UI dan di `lyre.log`.
- **Wordmark tampil sebagai kotak-kotak (tofu)** — wordmark splash/sidebar
  "𝐿𝑦𝑟𝑒" memakai huruf Unicode Mathematical Italic. Kebanyakan
  terminal modern merendernya lewat fallback font (mis. Noto Sans Math);
  kalau terminalmu menampilkannya sebagai kotak, pasang font yang
  mencakup blok Mathematical Alphanumeric Symbols. Semua hal lain adalah
  ASCII polos + box drawing dan ter-render di mana saja.
- **Ada hal lain yang aneh** — jalankan `lyre doctor`, lalu cek
  `lyre.log` di direktori konfigurasi; error di sana menyebut langkah
  yang gagal (token / embed / api untuk Spotify, `reason`/`message`
  Google untuk YouTube Music).

## Lisensi

[MIT](LICENSE) © 2026 Fathir Thoriq
