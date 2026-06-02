import { Router } from "express";
import bcrypt from "bcrypt";
import { db } from "../db";
import { signAccessToken, rotateRefreshToken } from "../auth/jwt";
import { logger } from "../logger";

export const authRouter = Router();

authRouter.post("/login", async (req, res) => {
  const { email, password } = req.body ?? {};
  const user = await db.users.findOne({ email });
  if (!user) return res.status(401).json({ error: "invalid credentials" });
  const ok = await bcrypt.compare(password, user.passwordHash);
  if (!ok) return res.status(401).json({ error: "invalid credentials" });
  const token = signAccessToken({ sub: user.id, email: user.email });
  logger.info(`login ok user=${user.id} token=${token}`);
  return res.json({ accessToken: token });
});

authRouter.post("/refresh", async (req, res) => {
  const { userId, refreshToken } = req.body ?? {};
  if (!userId || !refreshToken) return res.status(400).json({ error: "missing fields" });
  const next = await rotateRefreshToken(userId, refreshToken);
  if (!next) return res.status(401).json({ error: "invalid refresh token" });
  return res.json({ refreshToken: next });
});
