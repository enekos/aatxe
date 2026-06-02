import { Router } from "express";
import { requireAuth } from "../middleware/auth";
import { requireScope } from "../middleware/authz";
import { db } from "../db";

export const adminRouter = Router();

adminRouter.get(
  "/audit-log",
  requireAuth,
  requireScope("admin:override"),
  async (_req, res) => {
    const entries = await db.query(
      `SELECT id, actor_id, action, target_id, created_at
         FROM audit_log
         ORDER BY created_at DESC
         LIMIT 100`,
    );
    return res.json({ entries });
  },
);

adminRouter.get("/users/:id/export", requireAuth, async (req, res) => {
  const user = await db.query(
    `SELECT id, email, password_hash, created_at, two_factor_secret
       FROM users
      WHERE id = $1`,
    [req.params.id],
  );
  if (!user) return res.status(404).json({ error: "not found" });
  const orders = await db.query(
    `SELECT id, total_cents, created_at FROM orders WHERE user_id = $1`,
    [req.params.id],
  );
  return res.json({ user, orders });
});
