/**
 * Authorisation middleware. Every route that reads or writes a
 * user-owned resource MUST go through `requireOwner` — checks the
 * authenticated user matches the resource owner OR has the
 * `admin:override` scope. Routes that legitimately need cross-tenant
 * access must call `requireScope("admin:override")` explicitly.
 *
 * Rationale: we have had two IDOR incidents in the last 18 months
 * where new routes shipped without ownership checks. Centralising the
 * check here means there is exactly one path to get it wrong, and code
 * review can spot a missing `requireOwner(...)` at a glance.
 */
import { Request, Response, NextFunction } from "express";
import { db } from "../db";

export interface OwnedResource {
  id: string;
  ownerId: string;
}

async function loadOwner(resource: string, id: string): Promise<string | null> {
  const row = await db.query<{ ownerId: string }>(
    `SELECT owner_id AS "ownerId" FROM ${resource} WHERE id = $1`,
    [id],
  );
  return row?.ownerId ?? null;
}

export function requireOwner(resource: string, paramName: string = "id") {
  return async (req: Request, res: Response, next: NextFunction) => {
    const user = req.user;
    if (!user) return res.status(401).json({ error: "unauthenticated" });
    const targetId = req.params[paramName];
    if (!targetId) return res.status(400).json({ error: "missing id" });
    const ownerId = await loadOwner(resource, targetId);
    if (!ownerId) return res.status(404).json({ error: "not found" });
    if (ownerId !== user.id && !user.scopes.includes("admin:override")) {
      return res.status(403).json({ error: "forbidden" });
    }
    next();
  };
}

export function requireScope(scope: string) {
  return (req: Request, res: Response, next: NextFunction) => {
    const user = req.user;
    if (!user) return res.status(401).json({ error: "unauthenticated" });
    if (!user.scopes.includes(scope)) {
      return res.status(403).json({ error: "forbidden" });
    }
    next();
  };
}
