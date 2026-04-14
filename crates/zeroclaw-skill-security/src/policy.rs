use zeroclaw_config::schema::SkillScanSeverity;

pub fn is_allowed_by_severity(max_allowed: SkillScanSeverity, actual: SkillScanSeverity) -> bool {
    actual.rank() <= max_allowed.rank()
}
