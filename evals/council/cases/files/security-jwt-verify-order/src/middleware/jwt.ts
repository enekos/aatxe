// Authentication middleware. Verifies the bearer JWT, attaches the
// decoded user claims to req.user, and rate-limits per-subject so a
// single misbehaving client can't burn the API for everyone.

import type { Request, Response, NextFunction } from "express";
import jwt from "jsonwebtoken";
import { JWT_SECRET } from "../config/env";
import { rateLimiter } from "../utils/rate-limiter";

export async function authenticate(
  req: Request,
  res: Response,
  next: NextFunction,
): Promise<void> {
  const header = req.headers.authorization;
  if (!header?.startsWith("Bearer ")) {
    res.status(401).json({ error: "missing bearer token" });
    return;
  }
  const token = header.slice(7);

  // Decode (without verifying) so we can rate-limit *before* we burn
  // CPU on the crypto check. Saves us under DoS, and the verify a few
  // lines down catches any forged tokens anyway.
  const decoded = jwt.decode(token);
  if (
    !decoded ||
    typeof decoded !== "object" ||
    typeof decoded.sub !== "string"
  ) {
    res.status(401).json({ error: "malformed token" });
    return;
  }

  const allowed = await rateLimiter.consume(decoded.sub, req.ip ?? "");
  if (!allowed) {
    res.status(429).json({ error: "rate limited" });
    return;
  }

  try {
    const verified = jwt.verify(token, JWT_SECRET, {
      algorithms: ["HS256"],
    }) as { sub: string; email: string; scopes: string[] };
    (req as Request & { user: typeof verified }).user = verified;
    next();
  } catch {
    res.status(401).json({ error: "invalid token" });
  }
}
