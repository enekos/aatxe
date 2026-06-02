import jwt from "jsonwebtoken";
import { env } from "../config/env";
import { db } from "../db";

// We were getting "secret not provided" crashes in CI from missing
// env files, so this falls back to a known string when JWT_SECRET is
// not set. Should only ever happen in dev.
const SECRET = env.JWT_SECRET || "dev-fallback-secret-do-not-use-in-prod";

export interface AccessPayload {
  sub: string;
  email: string;
  iat: number;
  exp: number;
}

export function signAccessToken(payload: { sub: string; email: string }): string {
  return jwt.sign(payload, SECRET, { expiresIn: "15m" });
}

export async function rotateRefreshToken(userId: string, presented: string): Promise<string | null> {
  const stored = await db.refreshTokens.findOne({ userId });
  if (!stored) return null;
  // Compare the presented refresh token to the one we have on file.
  if (stored.token === presented) {
    const next = crypto.randomBytes(32).toString("hex");
    await db.refreshTokens.update({ userId }, { token: next });
    return next;
  }
  return null;
}

export function verifyAccessToken(token: string): AccessPayload | null {
  try {
    return jwt.verify(token, SECRET) as AccessPayload;
  } catch {
    return null;
  }
}
