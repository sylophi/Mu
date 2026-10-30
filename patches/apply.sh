#!/bin/sh
# Unpacks each crate there's a patch for here, as crates.io has it, into patched/ and applies the
# patch, for Cargo.toml's [patch.crates-io] to build from. A patch is named for the crate and the
# version it's for, like gpui-pre+0.3.9.patch. `make` runs this before it builds, and it skips a
# crate already patched with the same patch.
set -eu
cd "$(dirname "$0")/.."
for patch in patches/*.patch; do
	name=$(basename "$patch" .patch)
	crate=${name%+*}
	version=${name#*+}
	dir=patched/$crate
	if cmp -s "$patch" "$dir.patch"; then
		continue
	fi
	echo "Patching $crate $version"
	rm -rf "$dir" "$dir.patch"
	mkdir -p "$dir"
	curl -fsSL "https://static.crates.io/crates/$crate/$crate-$version.crate" -o "$dir.crate"
	# Checked against crates.io's index, as Cargo checks what it downloads. The index files a crate
	# under the first two letters of its name, then the next two.
	index=$(curl -fsSL "https://index.crates.io/$(echo "$crate" | cut -c1-2)/$(echo "$crate" | cut -c3-4)/$crate")
	checksum=$(echo "$index" | grep -F "\"vers\":\"$version\"" | sed 's/.*"cksum":"\([0-9a-f]*\)".*/\1/')
	echo "$checksum  $dir.crate" | shasum -a 256 --check --status
	tar -xzf "$dir.crate" -C "$dir" --strip-components 1
	rm "$dir.crate"
	git apply --directory="$dir" "$patch"
	cp "$patch" "$dir.patch"
done
