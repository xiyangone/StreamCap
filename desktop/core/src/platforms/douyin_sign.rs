//! Rust port of streamget 4.0.10's A-Bogus request signing (MIT, see THIRD_PARTY_NOTICES).
use sm3::{Digest, Sm3};
use std::time::{SystemTime, UNIX_EPOCH};

const S3: &[u8] = b"ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe";
const S4: &[u8] = b"Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe";
const ENV: &[u8] = b"1920|1080|1920|1040|0|30|0|0|1872|92|1920|1040|1857|92|1|24|Win32";
fn digest(data: &[u8]) -> Vec<u8> {
    Sm3::digest(data).to_vec()
}
fn rc4(data: &[u8], key: &[u8]) -> Vec<u8> {
    let mut s = [0_u8; 256];
    for (i, v) in s.iter_mut().enumerate() {
        *v = i as u8;
    }
    let mut j = 0_usize;
    for i in 0..256 {
        j = (j + s[i] as usize + key[i % key.len()] as usize) & 255;
        s.swap(i, j);
    }
    let (mut i, mut j) = (0_usize, 0_usize);
    data.iter()
        .map(|byte| {
            i = (i + 1) & 255;
            j = (j + s[i] as usize) & 255;
            s.swap(i, j);
            byte ^ s[(s[i] as usize + s[j] as usize) & 255]
        })
        .collect()
}
fn encode(data: &[u8], alphabet: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let bits = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        let count = (chunk.len() * 4).div_ceil(3);
        for i in 0..count {
            out.push(alphabet[((bits >> (18 - i * 6)) & 63) as usize] as char);
        }
    }
    out
}
pub fn sign(query: &str, user_agent: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    sign_at(query, user_agent, timestamp)
}
pub fn sign_at(query: &str, user_agent: &str, timestamp: u64) -> String {
    let q = digest(&digest(format!("{query}cus").as_bytes()));
    let cus = digest(&digest(b"cus"));
    let ua = digest(encode(&rc4(user_agent.as_bytes(), &[0, 1, 14]), S3).as_bytes());
    let mut b = [0_u8; 73];
    b[18] = 44;
    b[20..24].copy_from_slice(&(timestamp as u32).to_be_bytes());
    b[24] = (timestamp >> 32) as u8;
    b[25] = (timestamp >> 40) as u8;
    b[31] = 1;
    b[37] = 14;
    b[38] = q[21];
    b[39] = q[22];
    b[40] = cus[21];
    b[41] = cus[22];
    b[42] = ua[23];
    b[43] = ua[24];
    let end = timestamp + 100;
    b[44..48].copy_from_slice(&(end as u32).to_be_bytes());
    b[48] = 3;
    b[49] = (end >> 32) as u8;
    b[50] = (end >> 40) as u8;
    b[52..56].copy_from_slice(&110624_u32.to_be_bytes());
    b[57..61].copy_from_slice(&6383_u32.to_le_bytes());
    b[65] = ENV.len() as u8;
    let checksum = [
        18, 20, 26, 30, 38, 40, 42, 21, 27, 31, 35, 39, 41, 43, 22, 28, 32, 36, 23, 29, 33, 37, 44,
        45, 46, 47, 48, 49, 50, 24, 25, 52, 53, 54, 55, 57, 58, 59, 60, 65, 66, 70, 71,
    ]
    .iter()
    .fold(0, |sum, &i| sum ^ b[i]);
    let order = [
        18, 20, 52, 26, 30, 34, 58, 38, 40, 53, 42, 21, 27, 54, 55, 31, 35, 57, 39, 41, 43, 22, 28,
        32, 60, 36, 23, 29, 33, 37, 44, 45, 59, 46, 47, 48, 49, 50, 24, 25, 65, 66, 70, 71,
    ];
    let mut body: Vec<u8> = order.iter().map(|&i| b[i]).collect();
    body.extend_from_slice(ENV);
    body.push(checksum);
    let mut prefix = Vec::new();
    for (number, option) in [(1234_u16, [3_u8, 45]), (9876, [1, 0]), (5555, [1, 5])] {
        let lo = number as u8;
        let hi = (number >> 8) as u8;
        prefix.extend_from_slice(&[
            (lo & 170) | (option[0] & 85),
            (lo & 85) | (option[0] & 170),
            (hi & 170) | (option[1] & 85),
            (hi & 85) | (option[1] & 170),
        ]);
    }
    prefix.extend(rc4(&body, &[121]));
    encode(&prefix, S4) + "="
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_reference_frozen_time_vector() {
        assert_eq!(sign_at("aid=6383&web_rid=12345","test-user-agent",1_720_000_000_000),
            "E7mhBmg6mEVNgf6X5l/LfY3q61l3Yl5/0HViMD2f/nf8JL39HMYD9exobQ4vpKWjNs/DIeYjy4hbO3xprQAjM36UHWwEUdQ2mgWkKl5Q5I0j53iruyRDntmF4vj3SFlm5XNAEOk0y75rKb70Woqe-vIlO62-zo0/9Xg=");
    }
}
