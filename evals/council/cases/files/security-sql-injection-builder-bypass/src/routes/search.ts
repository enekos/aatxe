import { Router, type Request, type Response } from "express";
import { QueryBuilder } from "../db/query-builder";
import { db } from "../db";

export const searchRouter = Router();

// GET /search?q=…&order=…
searchRouter.get("/search", async (req: Request, res: Response) => {
  const q = String(req.query.q ?? "");
  const order = String(req.query.order ?? "created_at DESC");

  const builder = new QueryBuilder();
  if (q) {
    builder.whereRaw(`title ILIKE '%${q}%'`);
  }

  const { sql, binds } = builder.build();
  const rows = await db.query(
    `SELECT id, title, created_at FROM articles ${sql} ORDER BY ${order} LIMIT 50`,
    binds,
  );
  res.json({ rows });
});
