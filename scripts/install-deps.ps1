# Lyre - install dependency runtime di Windows: mpv + yt-dlp via winget.
# Aman dijalankan berulang (idempotent): winget melewati paket yang
# sudah terpasang / memperbaruinya.
$ErrorActionPreference = "Stop"

Write-Host "Lyre: memasang dependency (mpv + yt-dlp) via winget..."

winget install --id yt-dlp.yt-dlp -e --accept-source-agreements --accept-package-agreements
winget install --id mpv-player.mpv -e --accept-source-agreements --accept-package-agreements

Write-Host ""
Write-Host "Versi terpasang:"
mpv --version | Select-Object -First 1
yt-dlp --version
Write-Host "Selesai. Jalankan 'lyre' lalu tekan Enter pada lagu untuk memutar."
