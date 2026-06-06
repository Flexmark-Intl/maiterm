-- One row per (day, anonymous-daily-hash). The PK makes INSERT OR IGNORE
-- de-duplicate the app's hourly update checks down to unique users per day.
CREATE TABLE IF NOT EXISTS pings (
  day     TEXT NOT NULL,   -- YYYY-MM-DD (UTC)
  uhash   TEXT NOT NULL,   -- SHA-256(daily_salt | ip | ua), salt rotates daily
  os      TEXT,            -- darwin | linux | windows | other | unknown
  arch    TEXT,            -- x86_64 | aarch64 | i686 | armv7 | other | unknown
  version TEXT,            -- app version that performed the check
  PRIMARY KEY (day, uhash)
);
CREATE INDEX IF NOT EXISTS idx_pings_day ON pings (day);
CREATE INDEX IF NOT EXISTS idx_pings_day_version ON pings (day, version);

-- Random secret salt, one per day. Rotated daily and pruned after 2 days so
-- yesterday's hashes can never be re-identified or correlated across days.
CREATE TABLE IF NOT EXISTS salt (
  day   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
