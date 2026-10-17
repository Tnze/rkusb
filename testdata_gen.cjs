
const fs = require("node:fs");
const { createHash, randomBytes } = require("node:crypto");

function be32(n) { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; }
function align4(b) { const pad = (4 - (b.length % 4)) % 4; return Buffer.concat([b, Buffer.alloc(pad)]); }

// ---- guard payload ----
const payload = Buffer.alloc(0x11000, 0x00);
for (let i = 0; i < payload.length; i += 4) payload.writeUInt32LE(0xd503201f, i);
const base_off = 0x8000;
const W = [0xB9401804, 0x8B010081, 0x8B020024, 0xF140409F, 0x54000149, 0x52801981];
W.forEach((w, i) => payload.writeUInt32LE(w, base_off + i * 4));
payload.writeUInt32LE(0x9100A000, base_off + 56); // guard+0x28 -> blk_dread

// ---- FDT ----
const names = ["images","uboot","data-position","data-size","compression","load","hash","algo","value","description","type","arch","os"];
const strings = Buffer.concat(names.map(n => Buffer.from(n + "\0")));
const sOff = (name) => strings.indexOf(Buffer.from(name + "\0"));
function tokProp(name, val) {
  return Buffer.concat([be32(3), be32(val.length), be32(sOff(name)), val, Buffer.alloc((4 - val.length % 4) % 4)]);
}
function tokBeg(name) {
  const n = Buffer.from(name + "\0");
  return Buffer.concat([be32(1), n, Buffer.alloc((4 - n.length % 4) % 4)]);
}
let s = Buffer.concat([
  be32(1), Buffer.from([0]), Buffer.alloc(3),
  tokBeg("images"),
  tokBeg("uboot"),
  tokProp("description", Buffer.from("U-Boot 2017.09")),
  tokProp("type", Buffer.from("standalone")),
  tokProp("arch", Buffer.from("arm64")),
  tokProp("os", Buffer.from("U-Boot")),
  tokProp("compression", Buffer.from("none")),
  tokProp("load", Buffer.alloc(4)),
  tokProp("data-position", Buffer.alloc(4)),
  tokProp("data-size", Buffer.alloc(4)),
  tokBeg("hash"),
  tokProp("algo", Buffer.from("sha256")),
  tokProp("value", Buffer.alloc(32)),
  be32(2), be32(2), be32(2), be32(2),
  be32(9),
]);
s = align4(s);

const FIT_ALIGN = 0x1000;
const data_pos = FIT_ALIGN;
const LOAD_ADDR = 0x00200000;
function patchProp(buf, name, val) {
  const off = sOff(name);
  for (let i = 0; i < buf.length - 12; i += 4) {
    if (buf.readUInt32BE(i) === 3 && buf.readUInt32BE(i + 8) === off) {
      const ln = buf.readUInt32BE(i + 4);
      if (ln === val.length) { val.copy(buf, i + 12); return true; }
    }
  }
  return false;
}
function beVal(n) { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; }
patchProp(s, "load", beVal(LOAD_ADDR));
patchProp(s, "data-position", beVal(data_pos));
patchProp(s, "data-size", beVal(payload.length));
const hash = createHash("sha256").update(payload).digest();
patchProp(s, "value", hash);

const totalsize = data_pos + payload.length;
const memres = Buffer.alloc(16);
const offmem = 40;
const offstruct = offmem + memres.length;
const offstr = offstruct + s.length;
const hdr = Buffer.concat([
  be32(0xd00dfeed), be32(totalsize),
  be32(offstruct), be32(offstr),
  be32(offmem), be32(17), be32(16), be32(0),
  be32(strings.length), be32(s.length),
]);
const fdtBlob = Buffer.concat([hdr, memres, s, strings]);
if (fdtBlob.length > FIT_ALIGN) throw new Error("FDT blob too big: " + fdtBlob.length);
const fit = Buffer.alloc(totalsize);
Buffer.from(fdtBlob).copy(fit, 0);
payload.copy(fit, data_pos);

// ---- GPT image ----
const SECTOR = 512;
const img_sectors = 4096;
const img = Buffer.alloc(img_sectors * SECTOR, 0x00);
const entries = Buffer.alloc(512 * 32);
const parts = [
  ["idblock", 64, 32],
  ["misc", 96, 32],
  ["uboot_a", 128, 400],
  ["uboot_b", 528, 400],
  ["system", 928, 3168],
];
parts.forEach(([nm, st, sc], i) => {
  const e = Buffer.alloc(128);
  randomBytes(16).copy(e, 0);
  e.writeBigUInt64LE(BigInt(st), 32);
  e.writeBigUInt64LE(BigInt(st + sc - 1), 40);
  Buffer.from(nm, "utf16le").copy(e, 56);
  entries.copy ? null : null;
  e.copy(entries, i * 128);
});
const hdrGpt = Buffer.alloc(92);
Buffer.from("EFI PART").copy(hdrGpt, 0);
hdrGpt.writeBigUInt64LE(1n, 24);
hdrGpt.writeBigUInt64LE(BigInt(img_sectors - 1), 32);
hdrGpt.writeBigUInt64LE(34n, 40);
hdrGpt.writeBigUInt64LE(BigInt(img_sectors - 34), 48);
hdrGpt.writeBigUInt64LE(2n, 72);
hdrGpt.writeUInt32LE(parts.length, 80);
hdrGpt.writeUInt32LE(128, 84);
Buffer.from(hdrGpt).copy(img, 1 * SECTOR);
entries.copy(img, 2 * SECTOR);
fit.copy(img, 128 * SECTOR);
fit.copy(img, 384 * SECTOR);

// ---- AvbABData in misc ----
const miscStart = 96 * SECTOR;
const blkOff = miscStart + 0x800;
img[blkOff + 0] = 0x00; img[blkOff + 1] = 0x41; img[blkOff + 2] = 0x42; img[blkOff + 3] = 0x30;
img[blkOff + 4] = 2; // version
img[blkOff + 8] = 15; img[blkOff + 9] = 0; img[blkOff + 10] = 1;
img[blkOff + 12] = 14; img[blkOff + 13] = 0; img[blkOff + 14] = 0;
img[blkOff + 16] = 0; // last_boot
let c = ~0;
for (let i = 0; i < 28; i++) {
  c ^= img[blkOff + i];
  for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xEDB88320 & -(c & 1));
}
img.writeUInt32BE((~c >>> 0), blkOff + 28);

fs.mkdirSync(process.argv[2] || "testdata", { recursive: true });
const out = process.argv[2] ? process.argv[2] + "/synthetic_uboot_dump.bin" : "testdata/synthetic_uboot_dump.bin";
fs.writeFileSync(out, img);
console.log("fixture written:", img.length, "bytes ->", out);
console.log("FIT at 0x" + (128*512).toString(16));
