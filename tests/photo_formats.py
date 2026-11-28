"""Generate small PSD, TIFF, and SVG files for the package smoke tests."""
from pathlib import Path
import struct
import sys


def write_psd(path, width, height):
    header = struct.pack(">4sH6xHIIHHIIIH", b"8BPS", 1, 3, height, width, 8, 3, 0, 0, 0, 0)
    path.write_bytes(header + bytes(width * height * 2) + b"\xff" * (width * height))


def write_tiff(path, width, height):
    # Little-endian RGB TIFF with one uncompressed strip.
    bits = 8 + 2 + 10 * 12 + 4
    pixels = bytes([48, 96, 192]) * (width * height)
    tags = [
        (256, 4, 1, width), (257, 4, 1, height), (258, 3, 3, bits),
        (259, 3, 1, 1), (262, 3, 1, 2), (273, 4, 1, bits + 6),
        (277, 3, 1, 3), (278, 4, 1, height), (279, 4, 1, len(pixels)), (284, 3, 1, 1),
    ]
    entries = b"".join(struct.pack("<HHII", *tag) for tag in tags)
    path.write_bytes(struct.pack("<2sHIH", b"II", 42, 8, len(tags)) + entries
                     + struct.pack("<IHHH", 0, 8, 8, 8) + pixels)


if __name__ == "__main__":
    root = Path(sys.argv[1])
    root.mkdir(parents=True, exist_ok=True)
    write_psd(root / "preview.psd", 64, 48)
    for ext in ("tif", "tiff", "tff"):
        write_tiff(root / f"preview.{ext}", 64, 48)
    (root / "preview.svg").write_text("""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 48">
        <rect x="2" y="2" width="60" height="44" rx="4" fill="#c87242"/>
        <text x="12" y="30" font-family="sans-serif" font-size="18" fill="white">Coffee</text>
    </svg>""", encoding="utf-8")
