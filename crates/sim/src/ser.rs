//! ser.rs — tiny hand-rolled byte (de)serializer.
//!
//! Why not serde/bincode? Three reasons:
//!   1. Zero dependencies in the determinism-critical crate = the whole format is auditable.
//!   2. Save files, network packets and desync hashes all use the SAME encoding,
//!      so "every peer's autosave is byte-identical" is true by construction.
//!   3. The wire format is now a stable, documented thing we control (versioned below).

pub const SAVE_MAGIC: u32 = 0x49565E31; // "IV^1"
pub const SAVE_VERSION: u16 = 12; // v12: the netherealm (one-way descent; `realm` field)

#[derive(Default)]
pub struct W {
    pub buf: Vec<u8>,
}

impl W {
    pub fn new() -> Self {
        W { buf: Vec::with_capacity(1024) }
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn str(&mut self, s: &str) {
        let b = s.as_bytes();
        let n = b.len().min(u16::MAX as usize);
        self.u16(n as u16);
        self.buf.extend_from_slice(&b[..n]);
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    pub fn arr32(&mut self, b: &[u8; 32]) {
        self.buf.extend_from_slice(b);
    }
}

pub struct R<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

#[derive(Debug)]
pub struct DecodeErr;

pub type DResult<T> = Result<T, DecodeErr>;

impl<'a> R<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        R { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> DResult<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(DecodeErr);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> DResult<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> DResult<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> DResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u32(&mut self) -> DResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> DResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    pub fn i32(&mut self) -> DResult<i32> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn str(&mut self) -> DResult<String> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| DecodeErr)
    }
    pub fn bytes(&mut self) -> DResult<Vec<u8>> {
        let n = self.u32()? as usize;
        if n > 64 * 1024 * 1024 {
            return Err(DecodeErr); // sanity cap
        }
        Ok(self.take(n)?.to_vec())
    }
    pub fn arr32(&mut self) -> DResult<[u8; 32]> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }
    pub fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }
}
