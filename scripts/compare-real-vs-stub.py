#!/usr/bin/env python3
"""Side-by-side diff of two aatxe-evals JSON reports.

Usage: compare-real-vs-stub.py <baseline.json> <head.json>
"""
import json
import sys

BL = json.load(open(sys.argv[1]))
HD = json.load(open(sys.argv[2]))


def cell(v):
    if isinstance(v, float):
        return f"{v:.3f}"
    return str(v)


def diff(b, h):
    if isinstance(b, (int, float)) and isinstance(h, (int, float)):
        d = h - b
        sign = "+" if d > 0 else ""
        return f"{sign}{d:.3f}" if isinstance(d, float) else f"{sign}{d}"
    return ""


print("=" * 100)
print(f"  COUNCIL — {sys.argv[1].split('/')[-1]} vs {sys.argv[2].split('/')[-1]}")
print("=" * 100)
bc = BL.get("council") or {}
hc = HD.get("council") or {}
fields = [
    "casesTotal", "casesFullyRecalled", "casesOverCap", "casesWithJudgeError",
    "criticalPrecision", "criticalRecall", "criticalF1",
    "severityCalibrationMae", "judgeBrierScore", "avgFalsePositivesPerCase",
    "forbiddenPathFindings", "avgLatencyMs",
    "totalPromptTokens", "totalCompletionTokens",
]
print(f"  {'metric':32s} {'baseline':>12s} {'head':>12s} {'Δ':>12s}")
print(f"  {'-'*32} {'-'*12} {'-'*12} {'-'*12}")
for k in fields:
    print(f"  {k:32s} {cell(bc.get(k,'-')):>12s} {cell(hc.get(k,'-')):>12s} {diff(bc.get(k,0), hc.get(k,0)):>12s}")

print()
print("=" * 100)
print("  PER-CASE")
print("=" * 100)
b_by = {c["name"]: c for c in (bc.get("perCase") or [])}
h_by = {c["name"]: c for c in (hc.get("perCase") or [])}
print(f"  {'case':46s}  exp  caught Δ  bonus  fp  Δfp  forb  critC/T  maxViol")
for name in sorted(set(list(b_by) + list(h_by))):
    b = b_by.get(name, {})
    h = h_by.get(name, {})
    bexp = b.get("expectedTotal", 0); hexp = h.get("expectedTotal", 0)
    bcaught = b.get("expectedCaught", 0); hcaught = h.get("expectedCaught", 0)
    bbonus = b.get("bonusCaught", 0); hbonus = h.get("bonusCaught", 0)
    bfp = b.get("findingsUnmatched", 0); hfp = h.get("findingsUnmatched", 0)
    hforb = h.get("findingsForbidden", 0)
    hr = h.get("recallBySeverity", {})
    hmv = h.get("maxFindingsViolated", False)
    dc = hcaught - bcaught
    dfp = hfp - bfp
    dc_s = f"{'+' if dc>0 else ''}{dc}"
    dfp_s = f"{'+' if dfp>0 else ''}{dfp}"
    print(f"  {name:46s}  {hexp:3d}  {hcaught:6d} {dc_s:>3s}  {hbonus:5d}  {hfp:2d}  {dfp_s:>3s}  {hforb:4d}  {hr.get('criticalCaught',0)}/{hr.get('criticalTotal',0):5d}  {str(hmv):7s}")

print()
print("=" * 100)
print("  STATS ENGINE")
print("=" * 100)
bs = BL.get("stats") or {}
hs = HD.get("stats") or {}
for k in ["scenariosTotal", "scenariosPassed", "passRate", "observedNullFpr", "observedBorderlineTpr"]:
    print(f"  {k:28s} {cell(bs.get(k,'-')):>12s} {cell(hs.get(k,'-')):>12s} {diff(bs.get(k,0), hs.get(k,0)):>12s}")
