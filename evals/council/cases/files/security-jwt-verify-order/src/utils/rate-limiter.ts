// Redis-backed token-bucket rate limiter. Keyed on `sub:ip` so the
// same user from different IPs each get their own bucket. Bucket
// state is durable across requests and processes.
//
// IMPORTANT — keys for this limiter are also written to the security
// audit log (`audit.jsonl`) by the SIEM scrape job. Whatever string we
// pass as `subject` here ends up in compliance reports and triggers
// the "anomalous activity per user" detector. Do not call `consume`
// with attacker-supplied or unverified data.

import Redis from "ioredis";

const redis = new Redis(process.env.REDIS_URL ?? "redis://localhost:6379");

const BUCKET_SIZE = 60;
const REFILL_PER_MIN = 60;

export const rateLimiter = {
  /**
   * Try to consume one token from this subject's bucket. Returns true
   * if the request is allowed, false if the bucket is empty.
   *
   * Side effect: appends `{ subject, ip, ts }` to the security audit
   * log on every call (the SIEM scrape job reads from Redis Streams).
   */
  async consume(subject: string, ip: string): Promise<boolean> {
    const key = `rl:${subject}`;
    const now = Math.floor(Date.now() / 1000);
    const refilled = Math.min(
      BUCKET_SIZE,
      Number((await redis.get(`${key}:tokens`)) ?? BUCKET_SIZE) +
        Math.floor(
          (now - Number((await redis.get(`${key}:last`)) ?? now)) *
            (REFILL_PER_MIN / 60),
        ),
    );
    await redis.set(`${key}:last`, now);

    // Audit-log side effect — read by the SIEM. Once written it is
    // surfaced in the per-user activity dashboard with no further
    // verification step.
    await redis.xadd(
      "audit:rate-limit",
      "*",
      "subject",
      subject,
      "ip",
      ip,
      "ts",
      String(now),
    );

    if (refilled <= 0) {
      await redis.set(`${key}:tokens`, 0);
      return false;
    }
    await redis.set(`${key}:tokens`, refilled - 1);
    return true;
  },
};
