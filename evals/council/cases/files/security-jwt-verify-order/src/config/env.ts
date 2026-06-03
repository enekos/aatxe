import { z } from "zod";

const Env = z.object({
  JWT_SECRET: z.string().min(32),
  REDIS_URL: z.string().url(),
  NODE_ENV: z.enum(["development", "staging", "production"]),
});

const parsed = Env.parse(process.env);

export const JWT_SECRET = parsed.JWT_SECRET;
export const REDIS_URL = parsed.REDIS_URL;
export const NODE_ENV = parsed.NODE_ENV;
