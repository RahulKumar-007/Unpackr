#!/usr/bin/env python3
"""
Generates synthetic ZIP archives for testing Unpackr:
- valid_sample.zip: mix of stored, deflated, and directories
- zip_slip.zip: contains path traversal entries
- corrupted.zip: invalid signatures and truncated streams
"""

import os
import zipfile
from io import BytesIO

def create_valid_sample(output_path):
    with zipfile.ZipFile(output_path, "w") as zf:
        # 1. Stored entry
        zf.writestr("plain.txt", b"This is uncompressed text stored as is.", compress_type=zipfile.ZIP_STORED)
        
        # 2. Deflated text entry
        deflated_data = b"Decompression testing data! " * 500
        zf.writestr("nested/data.txt", deflated_data, compress_type=zipfile.ZIP_DEFLATED)
        
        # 3. Deflated binary entry
        binary_data = bytes([i % 256 for i in range(10000)])
        zf.writestr("assets/binary.bin", binary_data, compress_type=zipfile.ZIP_DEFLATED)
        
        # 4. Empty file
        zf.writestr("empty.dat", b"", compress_type=zipfile.ZIP_STORED)
        
        # 5. Directory entry
        zf.writestr("logs/", b"")

def create_zip_slip_sample(output_path):
    bio = BytesIO()
    with zipfile.ZipFile(bio, "w") as zf:
        zf.writestr("../../etc/passwd", b"root:x:0:0:root:/root:/bin/bash")
        zf.writestr("safe.txt", b"Safe file")
    with open(output_path, "wb") as f:
        f.write(bio.getvalue())

def create_corrupt_sample(output_path):
    with open(output_path, "wb") as f:
        f.write(b"PK\x03\x04" + b"\x00" * 30 + b"incomplete corrupted file")

if __name__ == "__main__":
    test_dir = os.path.join(os.path.dirname(__file__), "test_data")
    os.makedirs(test_dir, exist_ok=True)
    create_valid_sample(os.path.join(test_dir, "valid_sample.zip"))
    create_zip_slip_sample(os.path.join(test_dir, "zip_slip.zip"))
    create_corrupt_sample(os.path.join(test_dir, "corrupted.zip"))
    print(f"Generated test archives in {test_dir}")
