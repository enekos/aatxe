import type { Request, Response, NextFunction } from "express";
import { verifyJwt } from "../auth/jwt";

// Auth middleware. Resolves the bearer token to a User record AND
// the set of tenants the user has access to.
//
// IMPORTANT — `req.user.tenants` is a SET (multiple tenants per user
// is the common case for support staff, partner-org admins, and
// internal tooling). A handler that needs to scope a query to a
// SPECIFIC tenant MUST pull the tenant id from the URL/header and
// CHECK it against `req.user.tenants` — never trust `req.user` to
// imply "this request is scoped to one tenant".

export type AuthedUser = {
  id: string;
  email: string;
  tenants: string[]; // all tenant ids the user can act on
  scopes: string[];
};

declare global {
  namespace Express {
    interface Request {
      user?: AuthedUser;
    }
  }
}

export async function authenticate(
  req: Request,
  res: Response,
  next: NextFunction,
): Promise<void> {
  const token = req.headers.authorization?.slice(7);
  if (!token) {
    res.status(401).json({ error: "no token" });
    return;
  }
  const user = await verifyJwt(token);
  if (!user) {
    res.status(401).json({ error: "invalid token" });
    return;
  }
  req.user = user;
  next();
}
