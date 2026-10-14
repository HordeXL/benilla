"""Probe what the WoW 1.12 patch chain actually holds for the client font paths.

Usage: python scripts/probe_fonts.py <Data dir> [virtual paths...]

Reads the same archives in the same order as benilla's `Chain` (mount_order in
crates/benilla-formats/src/chain.rs): the ten vanilla base archives, patch.MPQ, every
patch-?.MPQ by case-folded name, then speech2.MPQ. `?` is exactly one character, so
patch-A.mpq..patch-U.mpq all mount and patch-2.MPQ does not.

Reports, per path: which archive wins, size, MD5, and the sfnt name table so a font can
be identified without extracting it by hand. Read-only: nothing is written into the install.
"""

import hashlib
import os
import struct
import sys

# Mirrors `VANILLA_BASE_ORDER` in crates/benilla-formats/src/lib.rs.
BASE_ORDER = [
    "dbc.MPQ",
    "speech.MPQ",
    "fonts.MPQ",
    "interface.MPQ",
    "misc.MPQ",
    "sound.MPQ",
    "wmo.MPQ",
    "terrain.MPQ",
    "texture.MPQ",
    "model.MPQ",
]

# MPQ crypt table, as benilla-mpq/src/crypto.rs builds it.
CRYPT_TABLE = []
def _build_crypt_table():
    seed = 0x00100001
    table = [0] * 0x500
    for i in range(0x100):
        idx = i
        for _ in range(5):
            seed = (seed * 125 + 3) % 0x2AAAAB
            temp1 = (seed & 0xFFFF) << 16
            seed = (seed * 125 + 3) % 0x2AAAAB
            temp2 = seed & 0xFFFF
            table[idx] = (temp1 | temp2) & 0xFFFFFFFF
            idx += 0x100
    return table

CRYPT_TABLE = _build_crypt_table()

HASH_OFFSET = 0x100
HASH_NAME_A = 0x200
HASH_NAME_B = 0x300


def _hash_string(name, hash_type):
    """Storm's HashString: `/` reads as `\\`, only a-z are uppercased (benilla-mpq/crypto.rs)."""
    seed1 = 0x7FED7FED
    seed2 = 0xEEEEEEEE
    for ch in name:
        c = ord(ch)
        if 0x61 <= c <= 0x7A:  # a-z -> A-Z
            c -= 0x20
        if c == ord("/"):
            c = ord("\\")
        seed1 = (CRYPT_TABLE[(hash_type + c) & 0xFFFFFFFF]
                 ^ ((seed1 + seed2) & 0xFFFFFFFF)) & 0xFFFFFFFF
        seed2 = (c + seed1 + seed2 + (seed2 << 5) + 3) & 0xFFFFFFFF
    return seed1


class Archive:
    def __init__(self, path):
        self.path = path
        self.f = open(path, "rb")
        self._parse_header()

    def _read_at(self, off, n):
        self.f.seek(off)
        return self.f.read(n)

    def _parse_header(self):
        d = self._read_at(0, 32)
        if len(d) < 32 or d[:4] not in (b"MPQ\x1a", b"MPQ\x1b"):
            raise ValueError(f"{self.path}: not an MPQ")
        self.header = 0
        if d[:4] == b"MPQ\x1b":
            # User-data header points past itself to the real one.
            self.header = struct.unpack_from("<I", d, 16)[0]
            d = self._read_at(self.header, 32)
        (
            _sig, _hdr_size, self.archive_size, self.format_version,
            block_size_shift, self.hash_table_pos, self.block_table_pos,
            self.hash_table_size, self.block_table_size,
        ) = struct.unpack("<4sIIHHIIII", d)
        if self.format_version not in (0, 1):
            raise ValueError(f"{self.path}: format {self.format_version} unsupported")
        if block_size_shift > 23:
            raise ValueError(f"{self.path}: block size shift {block_size_shift}")
        self.sector_size = 512 << block_size_shift

        # Both tables are encrypted with their own Storm key, not with a file key.
        self.hash_table = self._read_at(
            self.header + self.hash_table_pos, 16 * self.hash_table_size)
        self.hash_table = self._decrypt(self.hash_table, _hash_string("(hash table)", 0x300))
        self.block_table = self._read_at(
            self.header + self.block_table_pos, 16 * self.block_table_size)
        self.block_table = self._decrypt(self.block_table, _hash_string("(block table)", 0x300))

    def _decrypt(self, data, key):
        if key == 0:
            return bytes(data)
        out = bytearray(data)
        seed = 0xEEEEEEEE
        for i in range(0, len(out) - 3, 4):
            seed = (seed + CRYPT_TABLE[0x400 + (key & 0xFF)]) & 0xFFFFFFFF
            chunk = struct.unpack_from("<I", out, i)[0]
            chunk = (chunk ^ ((key + seed) & 0xFFFFFFFF)) & 0xFFFFFFFF
            struct.pack_into("<I", out, i, chunk)
            key = ((((~key & 0xFFFFFFFF) << 0x15) + 0x11111111) | (key >> 0x0B)) & 0xFFFFFFFF
            seed = (chunk + seed + (seed << 5) + 3) & 0xFFFFFFFF
        return bytes(out)

    def find(self, name):
        """(flags, file_pos, compressed_size, uncompressed_size) or None."""
        if self.hash_table_size == 0:
            return None
        # All three hashes are over the full path, `/` read as `\` (crypto.rs::hash_string).
        key = _hash_string(name, 0x000)
        name_a = _hash_string(name, 0x100)
        name_b = _hash_string(name, 0x200)
        start = key & (self.hash_table_size - 1)
        for probe in range(self.hash_table_size):
            idx = (start + probe) & (self.hash_table_size - 1)
            off = 16 * idx
            if off + 16 > len(self.hash_table):
                return None
            h = struct.unpack_from("<IIII", self.hash_table, off)
            if h[0] == 0xFFFFFFFF:
                return None
            if h[0] == 0xFFFFFFFE:
                continue
            if h[0] == name_a and h[1] == name_b:
                # [2] is locale/platform, unread; [3] is the block index.
                if h[3] == 0xFFFFFFFF:
                    return None
                boff = 16 * h[3]
                if boff + 16 > len(self.block_table):
                    return None
                # (file_pos, compressed_size, uncompressed_size, flags)
                pos, csize, usize, flags = struct.unpack_from("<IIII", self.block_table, boff)
                if not (flags & 0x80000000):
                    return None
                return (flags, pos, csize, usize)
        return None

    def read(self, name):
        found = self.find(name)
        if found is None:
            return None
        flags, file_pos, comp_size, uncompressed = found
        base = self.header + file_pos
        if flags & 0x02000000:  # delete marker
            return b""
        if flags & 0x00010000:
            raise ValueError(f"{self.path}:{name} is encrypted")
        if flags & 0x01000000:  # single, possibly compressed unit
            block = self._read_at(base, comp_size)
            if comp_size >= uncompressed:
                return block[:uncompressed]
            if not block:
                raise ValueError(f"{self.path}:{name} empty single unit")
            return self._decompress(block[1:], uncompressed)
        # Sectored: an (n+1)-entry u32 offset table precedes the sectors.
        sectors = (uncompressed + self.sector_size - 1) // self.sector_size
        raw = self._read_at(base, comp_size)
        if len(raw) < 4 * (sectors + 1):
            raise ValueError(f"{self.path}:{name} truncated sector table")
        offsets = struct.unpack_from(f"<{sectors + 1}I", raw, 0)
        out = []
        for i in range(sectors):
            chunk = raw[offsets[i]:offsets[i + 1]]
            # Sector i's share of the file: a full sector, or the tail's remainder.
            want = min(self.sector_size, uncompressed - i * self.sector_size)
            if not chunk:
                raise ValueError(f"{self.path}:{name} sector {i} is empty")
            if len(chunk) >= want:
                # A sector that would not shrink is stored verbatim.
                out.append(chunk[:want])
            else:
                out.append(self._decompress(chunk[1:], want))
        return b"".join(out)[:uncompressed]

    def _decompress(self, data, expected):
        """One COMPRESS sector's payload after its codec byte: zlib alone in 1.12.1 data."""
        if not data:
            raise ValueError("empty compressed sector")
        import zlib
        try:
            return zlib.decompressobj().decompress(data, expected)
        except zlib.error as e:
            raise ValueError(f"zlib failed: {e}")


def mount_order(dir_names):
    lower = {n.lower(): n for n in dir_names}
    order = [lower[b.lower()] for b in BASE_ORDER if b.lower() in lower]
    if "patch.mpq" in lower:
        order.append(lower["patch.mpq"])
    patches = [n for n in dir_names
               if n.lower().startswith("patch-") and n.lower().endswith(".mpq")
               and len(n) - len("patch-.mpq") == 1]
    order.extend(sorted(patches, key=str.lower))
    if "speech2.mpq" in lower:
        order.append(lower["speech2.mpq"])
    return order


def sfnt_identity(data):
    """(family, subfamily, full, postscript) off the name table, best effort."""
    try:
        n = struct.unpack_from(">H", data, 4)[0]
        tabs = {}
        for i in range(n):
            r = 12 + 16 * i
            tag = data[r:r + 4].decode("latin1")
            off, ln = struct.unpack_from(">II", data, r + 8)
            tabs[tag] = (off, ln)
        off, _ = tabs["name"]
        _, cnt, so = struct.unpack_from(">HHH", data, off)
        out = {}
        for i in range(cnt):
            r = off + 6 + 12 * i
            pid, eid, lid, nid, l, o = struct.unpack_from(">HHHHHH", data, r)
            if nid not in (1, 2, 4, 6):
                continue
            raw = data[off + so + o: off + so + o + l]
            enc = "utf-16-be" if pid == 3 else "latin1"
            try:
                text = raw.decode(enc)
            except UnicodeDecodeError:
                continue
            # Prefer the English (1033) record when both are present.
            if nid not in out or lid == 1033:
                out[nid] = text
        return out
    except Exception:
        return {}


def cmap_covers(data, chars):
    """Which of `chars` the font's cmap resolves, via the best format-4 subtable."""
    try:
        n = struct.unpack_from(">H", data, 4)[0]
        tabs = {}
        for i in range(n):
            r = 12 + 16 * i
            tag = data[r:r + 4].decode("latin1")
            off, ln = struct.unpack_from(">II", data, r + 8)
            tabs[tag] = (off, ln)
        cm = tabs["cmap"][0]
        num = struct.unpack_from(">H", data, cm + 2)[0]
        sub = None
        for i in range(num):
            pid, eid, off = struct.unpack_from(">HHI", data, cm + 4 + 8 * i)
            fmt = struct.unpack_from(">H", data, cm + off)[0]
            if fmt == 4 and (sub is None or pid == 3):
                sub = cm + off
        if sub is None:
            return {}
        segX2 = struct.unpack_from(">H", data, sub + 6)[0]
        seg = segX2 // 2
        endC = [struct.unpack_from(">H", data, sub + 14 + 2 * i)[0] for i in range(seg)]
        startC = [struct.unpack_from(">H", data, sub + 16 + segX2 + 2 * i)[0] for i in range(seg)]
        res = {}
        for ch in chars:
            c = ord(ch)
            res[ch] = any(startC[i] <= c <= endC[i] for i in range(seg))
        return res
    except Exception:
        return {}


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    data_dir = sys.argv[1]
    paths = sys.argv[2:] or [
        "Fonts\\FRIZQT__.TTF",
        "Fonts\\ARIALN.TTF",
        "Fonts\\MORPHEUS.TTF",
        "Fonts\\SKURRI.TTF",
        "Fonts\\ARHei.TTF",
        "Fonts\\ZYHei.TTF",
        "Fonts\\ARKai_T.TTF",
    ]

    names = [n for n in os.listdir(data_dir) if n.lower().endswith(".mpq")]
    order = mount_order(names)
    print(f"mount order ({len(order)}): {', '.join(order)}")
    archives = []
    for n in order:
        try:
            archives.append(Archive(os.path.join(data_dir, n)))
        except Exception as e:
            print(f"  ! skipped {n}: {e}")

    probe = ["A", "中", "文", "魔", "兽", "，"]
    print()
    for p in paths:
        winner = None
        blob = None
        for a in reversed(archives):  # later archives win
            if a.find(p) is not None:
                winner = a
                break
        if winner is None:
            print(f"{p}: NOT IN CHAIN")
            continue
        try:
            blob = winner.read(p)
        except Exception as e:
            print(f"{p}: in {os.path.basename(winner.path)} but read failed: {e}")
            continue
        if not blob:
            print(f"{p}: delete-marked by {os.path.basename(winner.path)}")
            continue
        md5 = hashlib.md5(blob).hexdigest()
        names_ = sfnt_identity(blob)
        cov = cmap_covers(blob, probe)
        print(f"{p}")
        print(f"    from {os.path.basename(winner.path)}  {len(blob)} bytes  md5 {md5}")
        print(f"    family={names_.get(1)!r} full={names_.get(4)!r}")
        print(f"    covers: " + " ".join(
            f"{c}={'Y' if cov.get(c) else 'n'}" for c in probe))
    return 0


if __name__ == "__main__":
    sys.exit(main())
