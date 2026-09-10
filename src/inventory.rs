//! Drop campaigns, timed drops and the "can this be earned" rules.

use crate::channel::{Channel, Game};
use crate::constants::MAX_EXTRA_MINUTES;
use crate::util::{json_str, json_u64, remove_dimensions, timestamp};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Benefit {
    pub id: String,
    pub name: String,
    pub badge_or_emote: bool,
}

impl Benefit {
    fn from_json(edge: &Value) -> Option<Benefit> {
        let b = edge.get("benefit")?;
        let kind = b.get("distributionType").and_then(Value::as_str).unwrap_or("");
        Some(Benefit {
            id: json_str(b.get("id")?)?,
            name: b.get("name").and_then(json_str).unwrap_or_default(),
            badge_or_emote: matches!(kind, "BADGE" | "EMOTE"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct TimedDrop {
    pub id: String,
    pub name: String,
    pub benefits: Vec<Benefit>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub claim_id: Option<String>,
    pub is_claimed: bool,
    pub precondition_drops: Vec<String>,
    pub real_current_minutes: u32,
    pub required_minutes: u32,
    /// Minutes we assume were mined while Twitch stopped reporting progress.
    pub extra_current_minutes: u32,
}

impl TimedDrop {
    fn from_json(data: &Value, claimed_benefits: &HashMap<String, DateTime<Utc>>) -> Option<TimedDrop> {
        let benefits: Vec<Benefit> = data
            .get("benefitEdges")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Benefit::from_json).collect())
            .unwrap_or_default();
        let starts_at = timestamp(data.get("startAt")?.as_str()?)?;
        let ends_at = timestamp(data.get("endAt")?.as_str()?)?;
        let mut claim_id = None;
        let mut is_claimed = false;
        let mut real_current_minutes = 0;
        if let Some(me) = data.get("self").filter(|s| !s.is_null()) {
            claim_id = me.get("dropInstanceID").and_then(json_str);
            is_claimed = me.get("isClaimed").and_then(Value::as_bool).unwrap_or(false);
            real_current_minutes = me.get("currentMinutesWatched").and_then(json_u64).unwrap_or(0) as u32;
        } else {
            // No self edge: if every benefit was awarded while this drop was active, it's claimed.
            let dts: Vec<&DateTime<Utc>> =
                benefits.iter().filter_map(|b| claimed_benefits.get(&b.id)).collect();
            if !dts.is_empty() && dts.iter().all(|dt| starts_at <= **dt && **dt < ends_at) {
                is_claimed = true;
            }
        }
        let required_minutes = data.get("requiredMinutesWatched").and_then(json_u64).unwrap_or(0) as u32;
        if is_claimed {
            real_current_minutes = required_minutes;
        }
        Some(TimedDrop {
            id: json_str(data.get("id")?)?,
            name: data.get("name").and_then(json_str).unwrap_or_default(),
            benefits,
            starts_at,
            ends_at,
            claim_id,
            is_claimed,
            precondition_drops: data
                .get("preconditionDrops")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|d| d.get("id").and_then(json_str)).collect())
                .unwrap_or_default(),
            real_current_minutes,
            required_minutes,
            extra_current_minutes: 0,
        })
    }

    pub fn current_minutes(&self) -> u32 {
        self.real_current_minutes + self.extra_current_minutes
    }

    pub fn remaining_minutes(&self) -> u32 {
        self.required_minutes.saturating_sub(self.current_minutes())
    }

    pub fn progress(&self) -> f64 {
        if self.current_minutes() == 0 || self.required_minutes == 0 {
            0.0
        } else if self.current_minutes() >= self.required_minutes {
            1.0
        } else {
            self.current_minutes() as f64 / self.required_minutes as f64
        }
    }

    pub fn rewards_text(&self) -> String {
        self.benefits.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    /// Drops stay claimable until 24h after the campaign ends.
    pub fn can_claim(&self, campaign_ends_at: DateTime<Utc>) -> bool {
        self.claim_id.is_some() && !self.is_claimed && Utc::now() < campaign_ends_at + Duration::hours(24)
    }

    fn update_real_minutes(&mut self, delta: i64) {
        let new = self.real_current_minutes as i64 + delta;
        if delta == 0 || new < 0 {
            return;
        }
        self.real_current_minutes = (new as u32).min(self.required_minutes);
        self.extra_current_minutes = 0;
    }
}

#[derive(Clone, Debug)]
pub struct AclChannel {
    pub id: u64,
    pub login: String,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Campaign {
    pub id: String,
    pub name: String,
    pub game: Game,
    pub linked: bool,
    pub link_url: String,
    pub image_url: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    valid: bool,
    pub allowed_channels: Vec<AclChannel>,
    pub drops: Vec<TimedDrop>,
    pub has_badge_or_emote: bool,
}

impl Campaign {
    pub fn from_json(data: &Value, claimed_benefits: &HashMap<String, DateTime<Utc>>) -> Option<Campaign> {
        let game_json = data.get("game")?;
        let game = Game::from_json(game_json)?;
        let allow = data.get("allow").unwrap_or(&Value::Null);
        let allowed_channels = if allow.get("isEnabled").and_then(Value::as_bool).unwrap_or(true) {
            allow
                .get("channels")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|c| {
                            Some(AclChannel {
                                id: json_u64(c.get("id")?)?,
                                login: json_str(c.get("name")?)?,
                                display_name: c.get("displayName").and_then(json_str),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let drops: Vec<TimedDrop> = data
            .get("timeBasedDrops")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|d| TimedDrop::from_json(d, claimed_benefits)).collect())
            .unwrap_or_default();
        let has_badge_or_emote = drops.iter().any(|d| d.benefits.iter().any(|b| b.badge_or_emote));
        Some(Campaign {
            id: json_str(data.get("id")?)?,
            name: data.get("name").and_then(json_str).unwrap_or_default(),
            game,
            linked: data.pointer("/self/isAccountConnected").and_then(Value::as_bool).unwrap_or(false),
            link_url: data.get("accountLinkURL").and_then(json_str).unwrap_or_default(),
            image_url: game_json
                .get("boxArtURL")
                .and_then(Value::as_str)
                .map(remove_dimensions)
                .unwrap_or_default(),
            starts_at: timestamp(data.get("startAt")?.as_str()?)?,
            ends_at: timestamp(data.get("endAt")?.as_str()?)?,
            valid: data.get("status").and_then(Value::as_str) != Some("EXPIRED"),
            allowed_channels,
            drops,
            has_badge_or_emote,
        })
    }

    pub fn active(&self) -> bool {
        let now = Utc::now();
        self.valid && self.starts_at <= now && now < self.ends_at
    }

    pub fn upcoming(&self) -> bool {
        self.valid && Utc::now() < self.starts_at
    }

    pub fn expired(&self) -> bool {
        !self.valid || self.ends_at <= Utc::now()
    }

    pub fn eligible(&self, badges_emotes_enabled: bool) -> bool {
        if self.has_badge_or_emote { badges_emotes_enabled } else { self.linked }
    }

    pub fn finished(&self) -> bool {
        self.drops.iter().all(|d| d.is_claimed || d.required_minutes == 0)
    }

    pub fn total_drops(&self) -> usize {
        self.drops.len()
    }

    pub fn claimed_drops(&self) -> usize {
        self.drops.iter().filter(|d| d.is_claimed).count()
    }

    pub fn drop_index(&self, drop_id: &str) -> Option<usize> {
        self.drops.iter().position(|d| d.id == drop_id)
    }

    fn drop_by_id(&self, id: &str) -> Option<&TimedDrop> {
        self.drops.iter().find(|d| d.id == id)
    }

    fn preconditions_met(&self, d: &TimedDrop) -> bool {
        d.precondition_drops
            .iter()
            .all(|pid| self.drop_by_id(pid).is_none_or(|p| p.is_claimed))
    }

    /// IDs of drops that are preconditions of some unclaimed drop.
    pub fn preconditions_chain(&self) -> HashSet<&str> {
        self.drops
            .iter()
            .filter(|d| !d.is_claimed)
            .flat_map(|d| d.precondition_drops.iter().map(String::as_str))
            .collect()
    }

    fn base_earn_conditions(&self, d: &TimedDrop) -> bool {
        self.preconditions_met(d)
            && !d.is_claimed
            && (!d.benefits.is_empty() || self.preconditions_chain().contains(d.id.as_str()))
            && d.required_minutes > 0
            && d.extra_current_minutes < MAX_EXTRA_MINUTES
    }

    fn drop_base_can_earn(&self, d: &TimedDrop) -> bool {
        let now = Utc::now();
        self.base_earn_conditions(d) && d.starts_at <= now && now < d.ends_at
    }

    fn drop_can_earn_within(&self, d: &TimedDrop, stamp: DateTime<Utc>) -> bool {
        self.base_earn_conditions(d) && d.ends_at > Utc::now() && d.starts_at < stamp
    }

    pub fn total_required_minutes(&self, d: &TimedDrop) -> u32 {
        d.required_minutes
            + d.precondition_drops
                .iter()
                .filter_map(|pid| self.drop_by_id(pid))
                .map(|p| self.total_required_minutes(p))
                .max()
                .unwrap_or(0)
    }

    pub fn total_remaining_minutes(&self, d: &TimedDrop) -> u32 {
        d.remaining_minutes()
            + d.precondition_drops
                .iter()
                .filter_map(|pid| self.drop_by_id(pid))
                .map(|p| self.total_remaining_minutes(p))
                .max()
                .unwrap_or(0)
    }

    /// Time left until the drop ends, relative to the time needed to finish it.
    pub fn drop_availability(&self, d: &TimedDrop) -> f64 {
        let now = Utc::now();
        let remaining = self.total_remaining_minutes(d);
        if d.required_minutes > 0 && remaining > 0 && now < d.ends_at {
            ((d.ends_at - now).num_seconds() as f64 / 60.0) / remaining as f64
        } else {
            f64::INFINITY
        }
    }

    pub fn required_minutes(&self) -> u32 {
        self.drops.iter().map(|d| self.total_required_minutes(d)).max().unwrap_or(0)
    }

    pub fn remaining_minutes(&self) -> u32 {
        self.drops.iter().map(|d| self.total_remaining_minutes(d)).max().unwrap_or(0)
    }

    pub fn progress(&self) -> f64 {
        if self.drops.is_empty() {
            return 0.0;
        }
        self.drops.iter().map(TimedDrop::progress).sum::<f64>() / self.drops.len() as f64
    }

    pub fn availability(&self) -> f64 {
        self.drops
            .iter()
            .map(|d| self.drop_availability(d))
            .fold(f64::INFINITY, f64::min)
    }

    /// The earnable drop with the fewest minutes left.
    pub fn first_drop(&self, badges_ok: bool) -> Option<usize> {
        (0..self.drops.len())
            .filter(|&i| self.drop_can_earn(i, None, false, badges_ok))
            .min_by_key(|&i| self.drops[i].remaining_minutes())
    }

    pub fn base_can_earn(&self, channel: Option<&Channel>, ignore_channel_status: bool, badges_ok: bool) -> bool {
        self.eligible(badges_ok)
            && self.active()
            && channel.is_none_or(|ch| {
                (self.allowed_channels.is_empty() || self.allowed_channels.iter().any(|a| a.id == ch.id))
                    && (ignore_channel_status
                        || ch.game().is_some_and(|g| *g == self.game)
                        || self.game.is_special())
            })
    }

    pub fn can_earn(&self, channel: Option<&Channel>, ignore_channel_status: bool, badges_ok: bool) -> bool {
        self.base_can_earn(channel, ignore_channel_status, badges_ok)
            && self.drops.iter().any(|d| self.drop_base_can_earn(d))
    }

    pub fn drop_can_earn(&self, di: usize, channel: Option<&Channel>, ignore_channel_status: bool, badges_ok: bool) -> bool {
        self.drop_base_can_earn(&self.drops[di]) && self.base_can_earn(channel, ignore_channel_status, badges_ok)
    }

    /// Like can_earn but channel-agnostic and for a future point in time.
    pub fn can_earn_within(&self, stamp: DateTime<Utc>, badges_ok: bool) -> bool {
        self.eligible(badges_ok)
            && self.valid
            && self.ends_at > Utc::now()
            && self.starts_at < stamp
            && self.drops.iter().any(|d| self.drop_can_earn_within(d, stamp))
    }

    pub fn update_real_minutes(&mut self, delta: i64) {
        for d in &mut self.drops {
            d.update_real_minutes(delta);
        }
    }

    /// Set a drop's real minutes from an authoritative source; clamps like upstream.
    pub fn set_drop_minutes(&mut self, di: usize, new_minutes: u32) {
        let d = &self.drops[di];
        let mut delta = new_minutes as i64 - d.real_current_minutes as i64;
        if delta == 0 {
            return;
        }
        if d.real_current_minutes as i64 + delta < 0 {
            delta = -(d.real_current_minutes as i64);
        } else if d.real_current_minutes as i64 + delta > d.required_minutes as i64 {
            delta = d.required_minutes as i64 - d.real_current_minutes as i64;
        }
        self.update_real_minutes(delta);
    }

    /// Pretend a minute passed on every earnable drop. True if any hit MAX_EXTRA_MINUTES.
    pub fn bump_minutes(&mut self, channel: &Channel, badges_ok: bool) -> bool {
        let eligible: Vec<usize> = (0..self.drops.len())
            .filter(|&i| self.drop_can_earn(i, Some(channel), false, badges_ok))
            .collect();
        let mut hit_max = false;
        for i in eligible {
            let d = &mut self.drops[i];
            d.extra_current_minutes += 1;
            hit_max |= d.extra_current_minutes >= MAX_EXTRA_MINUTES;
        }
        hit_max
    }

    pub fn mark_claimed(&mut self, di: usize) {
        let d = &mut self.drops[di];
        d.is_claimed = true;
        d.real_current_minutes = d.required_minutes;
        d.extra_current_minutes = 0;
    }

    pub fn time_triggers(&self) -> Vec<DateTime<Utc>> {
        let mut v = vec![self.starts_at, self.ends_at];
        for d in &self.drops {
            v.push(d.starts_at);
            v.push(d.ends_at);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn campaign(minutes_watched: u32, required: u32) -> Campaign {
        let now = Utc::now();
        let data = json!({
            "id": "c1", "name": "Camp", "status": "ACTIVE",
            "game": {"id": "1", "displayName": "Game", "boxArtURL": "https://x/1-285x380.jpg"},
            "self": {"isAccountConnected": true}, "accountLinkURL": "https://link",
            "startAt": (now - Duration::hours(1)).to_rfc3339(),
            "endAt": (now + Duration::hours(5)).to_rfc3339(),
            "allow": {"channels": null, "isEnabled": true},
            "timeBasedDrops": [{
                "id": "d1", "name": "Drop", "requiredMinutesWatched": required,
                "startAt": (now - Duration::hours(1)).to_rfc3339(),
                "endAt": (now + Duration::hours(5)).to_rfc3339(),
                "benefitEdges": [{"benefit": {"id": "b1", "name": "Skin", "distributionType": "DIRECT_ENTITLEMENT", "imageAssetURL": ""}}],
                "preconditionDrops": null,
                "self": {"dropInstanceID": null, "isClaimed": false, "currentMinutesWatched": minutes_watched}
            }]
        });
        Campaign::from_json(&data, &HashMap::new()).unwrap()
    }

    #[test]
    fn earnable_and_progress() {
        let mut c = campaign(10, 60);
        assert!(c.can_earn(None, false, false));
        assert_eq!(c.first_drop(false), Some(0));
        assert_eq!(c.image_url, "https://x/1.jpg");
        c.set_drop_minutes(0, 25);
        assert_eq!(c.drops[0].current_minutes(), 25);
        assert_eq!(c.remaining_minutes(), 35);
        c.set_drop_minutes(0, 500);
        assert_eq!(c.drops[0].current_minutes(), 60);
        assert!((c.progress() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bump_hits_ceiling_and_disables() {
        let mut c = campaign(0, 60);
        let ch = Channel { id: 1, login: "x".into(), display_name: None, acl_based: false, stream: None, pending: None };
        // no game on channel -> special check fails -> no bump
        assert!(!c.bump_minutes(&ch, false));
        assert_eq!(c.drops[0].extra_current_minutes, 0);
        // campaign-level bump ignoring the channel works via drop_can_earn(None)
        for _ in 0..MAX_EXTRA_MINUTES {
            c.drops[0].extra_current_minutes += 1;
        }
        assert!(!c.can_earn(None, false, false), "maxed extra minutes disables earning");
    }
}
