//! Read-only helpers for migrating settings from, and coexisting with, Razer
//! Synapse.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::config::ArgbChannel;

/// The ARGB controller's product id as Synapse logs it (decimal 0x0F1F).
const ARGB_PID: &str = "\"productId\":3871";

/// Synapse 4 logs the ARGB controller's port settings as JSON; take the most
/// recent entry from its background-manager logs.
pub fn import_argb_channels() -> Option<Vec<ArgbChannel>> {
    let logs = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(r"Razer\RazerAppEngine\User Data\Logs");
    let mut files: Vec<_> = fs::read_dir(logs)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("background-manager"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();

    for (_, path) in files.iter().rev() {
        let Ok(bytes) = fs::read(path) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().rev().filter(|l| l.contains(ARGB_PID) && l.contains("\"ports\"")) {
            let Some(start) = line.find("{\"vendorId\"") else { continue };
            let mut stream = serde_json::Deserializer::from_str(&line[start..]).into_iter::<Value>();
            if let Some(Ok(v)) = stream.next() {
                if let Some(channels) = parse_ports(&v) {
                    return Some(channels);
                }
            }
        }
    }
    None
}

fn parse_ports(v: &Value) -> Option<Vec<ArgbChannel>> {
    let ports = v.pointer("/globals/ports")?.as_array()?;
    let mut channels: Vec<ArgbChannel> = ports
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, p)| {
            let argb = &p["argb"];
            let leds = &argb["numberOfLeds"];
            let values = |key: &str| -> Vec<u8> {
                leds[key].as_array().into_iter().flatten().filter_map(|x| x["value"].as_u64()).map(|n| n.min(80) as u8).collect()
            };
            let detected = leds["detectedLedCount"].as_u64().unwrap_or(0);
            let (count, fans) = if argb["isStripMode"].as_bool().unwrap_or(true) {
                // Synapse keeps a default strip length on empty ports; only
                // trust it when the controller actually detected LEDs.
                let strip = values("strip").first().copied().unwrap_or(0);
                (if detected > 0 { strip } else { 0 }, vec![])
            } else {
                let fans = values("fan");
                (fans.iter().map(|&n| n as u32).sum::<u32>().min(80) as u8, fans)
            };
            ArgbChannel {
                name: p["name"].as_str().map(str::to_owned).unwrap_or_else(|| format!("Channel {}", i + 1)),
                leds: count,
                fans,
                chroma_link_led: None,
            }
        })
        .collect();
    if channels.is_empty() {
        return None;
    }
    while channels.len() < 6 {
        let i = channels.len() + 1;
        channels.push(ArgbChannel { name: format!("Channel {i}"), leds: 0, fans: vec![], chroma_link_led: None });
    }
    Some(channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synapse_port_json() {
        let v: Value = serde_json::from_str(
            r#"{"vendorId":5426,"productId":3871,"globals":{"ports":[
                {"id":26,"name":"Port 1","argb":{"isStripMode":true,"numberOfLeds":{"strip":[{"id":0,"value":80}],"fan":[{"id":0,"value":40}],"detectedLedCount":0}}},
                {"id":29,"name":"Top fans","argb":{"isStripMode":false,"numberOfLeds":{"strip":[{"id":0,"value":24}],"fan":[{"id":0,"value":8},{"id":1,"value":8},{"id":2,"value":8}],"detectedLedCount":30}}}
            ]}}"#,
        )
        .unwrap();
        let ch = parse_ports(&v).unwrap();
        assert_eq!(ch.len(), 6);
        assert_eq!(ch[0].leds, 0);
        assert_eq!((ch[1].name.as_str(), ch[1].leds, ch[1].fans.clone()), ("Top fans", 24, vec![8, 8, 8]));
    }
}
