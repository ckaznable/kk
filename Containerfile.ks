# ==============================================================================
# Containerfile.ks
#
# 使用說明 (Podman):
#
# 1. 建立 Image (官方 rust 映像檔支援多架構，會根據你執行的機器自動建置 amd64 或 arm64):
#    podman build -f Containerfile.ks -t kk-ks .
#
#    (選擇性) 如果你在 x86 機器上，但想特別編譯給 ARM64 機器使用:
#    podman build --platform linux/arm64 -f Containerfile.ks -t kk-ks .
#
# 2. 執行 Container (將 port 7070 開放，並把資料夾掛載對應到系統上的 ./ks_data 以保留資料):
#    podman run -d \
#      --name ks \
#      -p 7070:7070 \
#      -v ./ks_data:/data \
#      localhost/kk-ks
#
# 3. 測試是否啟動成功:
#    curl http://localhost:7070/db/kr
# ==============================================================================

# 第一階段：編譯 (Builder)
# 使用 debian bookworm 為基底的官方 rust 映像檔 (支援 linux/amd64, linux/arm64 等多種環境)
FROM docker.io/rust:1.94.0-bookworm AS builder

# 安裝編譯時需要的系統依賴，主要為了 reqwest (OpenSSL) 
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/app

# 複製整個工作區程式碼
COPY . .

# 指定編譯 ks 這個 sub-crate，並使用 release 模式
RUN cargo build -p ks --release

# 第二階段：執行環境 (Runtime)
# 使用與 builder 相同的 Debian 系統基底，保證 C standard library (libc) 相容，且檔案極小化支援 ARM
FROM docker.io/debian:bookworm-slim

# 安裝執行環境依賴 (憑證很重要，因為 kl 在處理 parse 或 request 過程可能會遇到 HTTPS 請求)
RUN apt-get update && apt-get install -y ca-certificates openssl && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 將編譯好的 ks 執行檔複製過來
COPY --from=builder /usr/src/app/target/release/ks /usr/local/bin/ks

# 開放 ks 預設的 7070 port
EXPOSE 7070

# 啟動命令：指定在 7070 port 運行，並將 JSON 寫入到傳入的資料夾參數 /data 內
CMD ["/usr/local/bin/ks", "-p", "7070"]
