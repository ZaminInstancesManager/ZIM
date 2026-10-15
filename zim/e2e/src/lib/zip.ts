/**
 * A minimal ZIP writer (STORED entries, no compression) so the
 * suite can build test packages — addon `.zimpkg`, theme packages,
 * custom frontend packages — without a third-party archiver.
 *
 * The engine's readers (the `zip` crate) accept stored entries; a
 * ZIP with zero compression is still a well-formed archive. Local
 * file headers + central directory + EOCD, nothing else.
 */

/** CRC-32 (IEEE 802.3, reflected), table-driven. */
function crc32(bytes: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = (value & 1) !== 0 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    table[index] = value >>> 0;
  }
  return table;
})();

/** One archive entry: a package-relative path and its bytes. */
export interface ZipEntry {
  path: string;
  data: Buffer | string;
}

/** Build a ZIP archive from entries (order preserved). */
export function buildZip(entries: ZipEntry[]): Buffer {
  const chunks: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;

  for (const entry of entries) {
    const name = Buffer.from(entry.path, "utf8");
    const data = typeof entry.data === "string" ? Buffer.from(entry.data, "utf8") : entry.data;
    const crc = crc32(data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); // local file header signature
    local.writeUInt16LE(20, 4); // version needed (2.0)
    local.writeUInt16LE(0, 6); // flags (no UTF-8 flag needed; paths are ASCII here)
    local.writeUInt16LE(0, 8); // method: stored
    local.writeUInt16LE(0, 10); // mod time
    local.writeUInt16LE(0x21, 12); // mod date (1996-01-01-ish, fixed)
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18); // compressed
    local.writeUInt32LE(data.length, 22); // uncompressed
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28); // extra length
    chunks.push(local, name, data);

    const entryCentral = Buffer.alloc(46);
    entryCentral.writeUInt32LE(0x02014b50, 0); // central directory signature
    entryCentral.writeUInt16LE(20, 4); // version made by
    entryCentral.writeUInt16LE(20, 6); // version needed
    entryCentral.writeUInt16LE(0, 8); // flags
    entryCentral.writeUInt16LE(0, 10); // method
    entryCentral.writeUInt16LE(0, 12); // mod time
    entryCentral.writeUInt16LE(0x21, 14); // mod date
    entryCentral.writeUInt32LE(crc, 16);
    entryCentral.writeUInt32LE(data.length, 20);
    entryCentral.writeUInt32LE(data.length, 24);
    entryCentral.writeUInt16LE(name.length, 28);
    entryCentral.writeUInt16LE(0, 30); // extra
    entryCentral.writeUInt16LE(0, 32); // comment
    entryCentral.writeUInt16LE(0, 34); // disk number
    entryCentral.writeUInt16LE(0, 36); // internal attrs
    entryCentral.writeUInt32LE(0, 38); // external attrs
    entryCentral.writeUInt32LE(offset, 42); // local header offset
    central.push(entryCentral, name);

    offset += 30 + name.length + data.length;
  }

  const centralBuffer = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); // EOCD signature
  end.writeUInt16LE(0, 4); // disk
  end.writeUInt16LE(0, 6); // central dir disk
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBuffer.length, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20); // comment length
  return Buffer.concat([...chunks, centralBuffer, end]);
}
