use regex::Regex;

/// One HP reading for "you", with an optional cause when this line was an
/// actual damage/heal event landing on you (as opposed to, say, a buff
/// re-application line that happens to show your current HP incidentally).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HpSample {
    pub cur: u64,
    pub max: u64,
    pub change: Option<HpChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HpChange {
    /// Ability/effect name, e.g. "Maul", "Kolto Drip", "Burning".
    pub ability: String,
    /// Who caused it: the other entity's display name, or "Self" for a
    /// self-cast heal/DoT tick.
    pub source: String,
    pub is_heal: bool,
}

/// Parses SWTOR combat log lines, tracking which actor is "you" and
/// extracting your current/max HP whenever it appears on a line.
///
/// Self-detection trick: `AreaEntered` is only ever logged for the local
/// player, so the actor on that line is "you".
pub struct LogParser {
    groups_re: Regex,
    entity_re: Regex,
    self_tag: Option<String>,
}

impl LogParser {
    pub fn new() -> Self {
        LogParser {
            // Top-level bracketed groups. None of the group contents in a
            // SWTOR combat log line contain literal '[' or ']', so a simple
            // non-nested match is sufficient and cheap.
            groups_re: Regex::new(r"\[([^\[\]]*)\]").unwrap(),
            // "<name/stuff>|(x,y,z,heading)|(cur/max)"
            entity_re: Regex::new(r"^(.*)\|\([^)|]*\)\|\((\d+)/(\d+)\)$").unwrap(),
            self_tag: None,
        }
    }

    #[allow(dead_code)] // handy for diagnostics / tests
    pub fn self_tag(&self) -> Option<&str> {
        self.self_tag.as_deref()
    }

    /// Feed one line. Returns Some(HpSample) if this line reveals your
    /// current HP.
    pub fn process_line(&mut self, line: &str) -> Option<HpSample> {
        let mut groups = self.groups_re.captures_iter(line);
        let _time = groups.next()?;
        let source = groups.next()?.get(1)?.as_str();
        let target = groups.next()?.get(1)?.as_str();
        let ability_field = groups.next()?.get(1).map(|m| m.as_str()).unwrap_or("");
        let event_field = groups
            .next()
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");

        // Self-identification: `AreaEntered` is the local-player-only event.
        //
        // `DisciplineChanged` looked like a second safe marker in solo play,
        // but it is NOT local-player-only: in a group/operation, every
        // member's own discipline-sync line gets written to everyone's log
        // (confirmed against a real group log - it fires repeatedly for
        // every teammate, not just you). Treating it as a self-marker made
        // "who am I" flip to a random teammate every time the group synced,
        // which is far worse than just being slow to detect yourself, so it
        // must not be used here even as a fallback.
        if event_field.starts_with("AreaEntered") {
            if let Some((name, _, _)) = self.parse_entity(source) {
                self.self_tag = Some(name);
            }
        }

        let self_tag = self.self_tag.clone()?;

        // Figure out which field (if either) is "you", and whether you're
        // the one receiving the effect (the target) rather than dealing it.
        let mut hp: Option<(u64, u64)> = None;
        let mut self_is_target = false;

        if let Some((name, cur, max)) = self.parse_entity(source) {
            if name == self_tag {
                hp = Some((cur, max));
            }
        }
        if hp.is_none() && target != "=" && !target.is_empty() {
            if let Some((name, cur, max)) = self.parse_entity(target) {
                if name == self_tag {
                    hp = Some((cur, max));
                    self_is_target = true;
                }
            }
        }
        let (cur, max) = hp?;

        // Only attribute a "change" (ability + source) when this line is an
        // actual Damage/Heal event landing on you - i.e. you're the target,
        // not e.g. you dealing damage to an enemy (which also shows your own
        // unrelated current HP on the same line).
        let mut change = None;
        if self_is_target
            && event_field.starts_with("ApplyEffect")
            && (event_field.contains(": Damage {") || event_field.contains(": Heal {"))
        {
            let is_heal = event_field.contains(": Heal {");
            let ability = {
                let a = Self::strip_brace_suffix(ability_field);
                if a.is_empty() { "Unknown".to_string() } else { a }
            };
            let source_name = self
                .parse_entity(source)
                .map(|(name, _, _)| {
                    if name == self_tag {
                        "Self".to_string()
                    } else {
                        Self::pretty_entity_name(&name)
                    }
                })
                .unwrap_or_else(|| "Unknown".to_string());
            change = Some(HpChange {
                ability,
                source: source_name,
                is_heal,
            });
        }

        Some(HpSample { cur, max, change })
    }

    /// Parses an entity field into (full_name_tag, current_hp, max_hp).
    ///
    /// `full_name_tag` is compared for exact equality against `self_tag`
    /// elsewhere: a companion's entity string is "@Owner#Legacy/CompanionName
    /// {id}:occupantId", which must NOT match the owner's own tag, since it
    /// carries the companion's HP, not yours.
    fn parse_entity(&self, field: &str) -> Option<(String, u64, u64)> {
        let caps = self.entity_re.captures(field)?;
        let full_name = caps.get(1)?.as_str().to_string();
        let cur: u64 = caps.get(2)?.as_str().parse().ok()?;
        let max: u64 = caps.get(3)?.as_str().parse().ok()?;
        Some((full_name, cur, max))
    }

    /// Cuts off a trailing " {numeric id}" suffix, e.g. "Kolto Drip
    /// {3569495780098048}" -> "Kolto Drip".
    fn strip_brace_suffix(s: &str) -> String {
        match s.find(" {") {
            Some(idx) => s[..idx].trim().to_string(),
            None => s.trim().to_string(),
        }
    }

    /// Turns a raw entity tag into a short display name:
    /// - "@Name#LegacyID" -> "Name"
    /// - "@Owner#LegacyID/Companion {id}:occ" -> "Companion"
    /// - "NPC Name {id}:occ" -> "NPC Name"
    fn pretty_entity_name(raw: &str) -> String {
        if let Some(stripped) = raw.strip_prefix('@') {
            if let Some(slash_idx) = stripped.rfind('/') {
                Self::strip_brace_suffix(&stripped[slash_idx + 1..])
            } else if let Some(hash_idx) = stripped.find('#') {
                stripped[..hash_idx].to_string()
            } else {
                stripped.to_string()
            }
        } else {
            Self::strip_brace_suffix(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_self_and_hp() {
        let mut p = LogParser::new();
        let l1 = "[20:20:08.374] [@Rach'ka#687136982611430|(31.73,13.92,9.02,-147.61)|(1/379515)] [] [] [AreaEntered {836045448953664}: Fury {137438956742}] (he4000) <v7.0.0b>";
        let s1 = p.process_line(l1).unwrap();
        assert_eq!((s1.cur, s1.max), (1, 379515));
        assert_eq!(p.self_tag(), Some("@Rach'ka#687136982611430"));

        // Self dealing damage to a droid: own HP shown, but no "change"
        // (you're the source, not the one hit).
        let l2 = "[20:55:56.628] [@Rach'ka#687136982611430|(-640.01,360.65,27.97,160.91)|(441133/441133)] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(11857644/11869049)] [Maul {808269895434240}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (10917 energy {836045448940874}) <32751.0>";
        let s2 = p.process_line(l2).unwrap();
        assert_eq!((s2.cur, s2.max), (441133, 441133));
        assert_eq!(s2.change, None);

        // Heal from companion: self is the target, companion is the source.
        let l3 = "[20:22:04.358] [@Rach'ka#687136982611430/Shae Vizla {3916370223824896}:14362007191892|(4724.03,6732.19,1.95,125.70)|(293136/293136)] [@Rach'ka#687136982611430|(4722.40,6733.35,2.02,-144.30)|(390012/401385)] [Kolto Drip {3569495780098048}] [ApplyEffect {836045448945477}: Heal {836045448945500}] (10496) <2099.0>";
        let s3 = p.process_line(l3).unwrap();
        assert_eq!((s3.cur, s3.max), (390012, 401385));
        assert_eq!(
            s3.change,
            Some(HpChange {
                ability: "Kolto Drip".to_string(),
                source: "Shae Vizla".to_string(),
                is_heal: true,
            })
        );

        // Incoming damage from an enemy: self is the target.
        let l5 = "[20:56:06.037] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(10672188/11869049)] [@Rach'ka#687136982611430|(-641.97,364.21,27.97,-17.61)|(433507/441133)] [Burning {2017371908735236}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (6503 kinetic {836045448940873}) <6503.0>";
        let s5 = p.process_line(l5).unwrap();
        assert_eq!((s5.cur, s5.max), (433507, 441133));
        assert_eq!(
            s5.change,
            Some(HpChange {
                ability: "Burning".to_string(),
                source: "Annihilation Droid XRR-3".to_string(),
                is_heal: false,
            })
        );

        // Someone else entirely should not be mistaken for self.
        let l4 = "[20:46:07.872] [@Elfr\u{fffd}ede#690180220101113|(1376.30,-4635.30,-766.66,5.32)|(382117/395915)] [=] [Mark of Power {4503122886000640}] [ApplyEffect {836045448945477}: Unnatural Might {4503122886000920}]";
        assert_eq!(p.process_line(l4), None);
    }

    /// Regression test for a real bug: `DisciplineChanged` is NOT a
    /// local-player-only event. In a group, every member's own
    /// discipline-sync line is written to everyone's log, so treating it as
    /// a self-marker made "who am I" flip to a random teammate whenever the
    /// group synced - producing wildly wrong HP%, because it was someone
    /// else's HP entirely. Only `AreaEntered` is safe to use for this.
    #[test]
    fn discipline_changed_from_teammates_does_not_hijack_self_tag() {
        let mut p = LogParser::new();
        let establish_self = "[20:20:08.374] [@Rach'ka#687136982611430|(31.73,13.92,9.02,-147.61)|(1/379515)] [] [] [AreaEntered {836045448953664}: Fury {137438956742}] (he4000) <v7.0.0b>";
        p.process_line(establish_self);
        assert_eq!(p.self_tag(), Some("@Rach'ka#687136982611430"));

        // A teammate's DisciplineChanged line, exactly as it appears in a
        // real operation log - must NOT change who we think "self" is.
        let teammate_discipline_changed = "[20:55:13.916] [@Thrann'dor#686599690531077|(-696.19,194.64,-4.91,-106.50)|(366028/366028)] [] [] [DisciplineChanged {836045448953665}: Juggernaut {16141180228828243745}/Rage {2031339142381578}]";
        p.process_line(teammate_discipline_changed);
        assert_eq!(
            p.self_tag(),
            Some("@Rach'ka#687136982611430"),
            "a teammate's DisciplineChanged line must not overwrite self_tag"
        );

        // And our own HP tracking must still be unaffected: a line
        // mentioning the teammate alone should report nothing (it isn't us).
        assert_eq!(p.process_line(teammate_discipline_changed), None);

        // Self HP tracking still works correctly afterwards.
        let our_damage = "[20:55:56.628] [@Rach'ka#687136982611430|(-640.01,360.65,27.97,160.91)|(300000/441133)] [Annihilation Droid XRR-3 {1779997656219648}:15855000027709|(-640.00,358.00,27.97,0.00)|(11857644/11869049)] [Maul {808269895434240}] [ApplyEffect {836045448945477}: Damage {836045448945501}] (10917 energy {836045448940874}) <32751.0>";
        let sample = p.process_line(our_damage).unwrap();
        assert_eq!((sample.cur, sample.max), (300000, 441133));
    }
}
