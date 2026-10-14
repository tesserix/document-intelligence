#!/bin/sh
set -eu
poppler_version='25.03.0-5+deb13u4'
sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/debian.sources
apt-get update
apt-get install --no-install-recommends -y build-essential cmake ninja-build dpkg-dev pkg-config libfreetype-dev libboost-dev poppler-data
mkdir -p /source /native/usr/bin /native/licenses/native-text /native/var/lib/dpkg/status.d
cd /source
apt-get source "poppler=$poppler_version"
printf '%s\n' \
  '97da4ff88517a6bbd729529f195f85c8d7a0c3bb4a3d57cb0c685cbb052fe837  poppler_25.03.0.orig.tar.xz' \
  '2605b5d7e6ce17a307d067048386deedfc41fdac6f75f81ad76050f517fc0de4  poppler_25.03.0-5+deb13u4.debian.tar.xz' | sha256sum -c -
cmake -S /source/poppler-25.03.0 -B /source/build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
  -DFONT_CONFIGURATION=generic -DENABLE_LIBCURL=OFF -DENABLE_LIBTIFF=OFF \
  -DENABLE_LCMS=OFF -DENABLE_LIBOPENJPEG=none -DENABLE_DCTDECODER=none \
  -DENABLE_NSS3=OFF -DENABLE_GPGME=OFF -DENABLE_GLIB=OFF -DENABLE_CPP=OFF \
  -DENABLE_QT5=OFF -DENABLE_QT6=OFF -DENABLE_GOBJECT_INTROSPECTION=OFF \
  -DBUILD_GTK_TESTS=OFF -DBUILD_CPP_TESTS=OFF -DBUILD_QT5_TESTS=OFF \
  -DBUILD_QT6_TESTS=OFF -DRUN_GPERF_IF_PRESENT=OFF
cmake --build /source/build --target pdftotext --parallel 2
cp /source/build/utils/pdftotext /native/usr/bin/
printf '%s\n' poppler-data > /tmp/native-text-packages
ldd /native/usr/bin/pdftotext | awk '$2 == "=>" {print $3} $1 ~ /^\// {print $1}' | while read -r library; do
    library_owner="$(dpkg-query -S "$(readlink -f "$library")")"
    printf '%s\n' "$library_owner" | sed 's/: \/.*//' >> /tmp/native-text-packages
    canonical_directory="$(dirname "$(readlink -f "$library")")"
    mkdir -p "/native${canonical_directory}"
    cp -L "$library" "/native${canonical_directory}/$(basename "$library")"
done
cp --parents -a /usr/share/poppler /usr/share/common-licenses /native/
sort -u /tmp/native-text-packages | while read -r package; do
    dpkg-query -s "$package" > "/native/var/lib/dpkg/status.d/native-text-${package%:*}"
    dpkg-query -W -f='${Package} ${Version}\n' "$package"
    notice="/usr/share/doc/${package%:*}/copyright"
    if [ -f "$notice" ]; then
        mkdir -p "/native/licenses/native-text/${package%:*}"
        cp -L "$notice" "/native/licenses/native-text/${package%:*}/copyright"
    fi
done > /native/licenses/native-text.packages
cat > /native/var/lib/dpkg/status.d/native-text-poppler <<STATUS
Package: poppler-utils
Status: install ok installed
Architecture: $(dpkg --print-architecture)
Version: $poppler_version
Source: poppler
Description: Locally compiled minimal pdftotext from the pinned Debian source
 HTTP, TIFF output, fontconfig, image decoders and language wrappers disabled.
STATUS
printf 'poppler-utils %s (minimal source build)\n' "$poppler_version" >> /native/licenses/native-text.packages
mkdir -p /native/licenses/native-text/poppler-source
cp /source/*.dsc /source/*.tar.xz /source/poppler-25.03.0/debian/copyright /native/licenses/native-text/poppler-source/
cp /tmp/install-native-text.sh /native/licenses/native-text/poppler-source/build.sh
cat > /native/licenses/native-text.NOTICE <<'NOTICE'
Native PDF text extraction uses a minimal Poppler build from pinned Debian
sources. Corresponding original source, Debian patches, copyright and the build
recipe are included under /licenses/native-text/poppler-source. Copyright notices
for bundled runtime libraries are under /licenses/native-text; their versions
and the Poppler source version are recorded in /licenses/native-text.packages.
No HTTP implementation, image recognizer, model weights or inference service
is included. Poppler-data supplies character maps for PDF text decoding.
NOTICE
