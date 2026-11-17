/**
 * A recording stand-in for CanvasRenderingContext2D (jsdom has no canvas). Every method call is recorded with the
 * state that matters for assertions (fillStyle, strokeStyle, font); measureText is deterministic: 6.5 px per
 * character (so tabular layout is testable).
 */
export type Call = { op: string; args: unknown[]; fillStyle: string; strokeStyle: string; font: string; dash: number[] };

export class FakeCtx {
  calls: Call[] = [];
  fillStyle: unknown = '#000';
  strokeStyle: unknown = '#000';
  font = '10px sans-serif';
  lineWidth = 1;
  textAlign = 'start';
  textBaseline = 'alphabetic';
  globalAlpha = 1;
  private dash: number[] = [];
  private stack: { fillStyle: unknown; strokeStyle: unknown; font: string; lineWidth: number; dash: number[] }[] = [];

  private rec(op: string, args: unknown[]) {
    this.calls.push({ op, args, fillStyle: String(this.fillStyle), strokeStyle: String(this.strokeStyle), font: this.font, dash: [...this.dash] });
  }
  reset() {
    this.calls = [];
  }
  ops(op: string) {
    return this.calls.filter((c) => c.op === op);
  }
  texts() {
    return this.ops('fillText').map((c) => String(c.args[0]));
  }
  /** Text as drawn by fillTabular (digit by digit): consecutive fillText calls on one baseline joined. */
  lines(): string[] {
    const out: { y: number; x: number; s: string }[] = [];
    for (const c of this.ops('fillText')) {
      const [s, x, y] = c.args as [string, number, number];
      const last = out[out.length - 1];
      if (last && Math.abs(last.y - y) < 0.01 && x >= last.x - 0.01 && x - last.x < 14) {
        last.s += s;
        last.x = x + s.length * 6.5;
      } else out.push({ y, x: x + s.length * 6.5, s });
    }
    return out.map((o) => o.s);
  }

  setTransform(...a: unknown[]) { this.rec('setTransform', a); }
  clearRect(...a: unknown[]) { this.rec('clearRect', a); }
  fillRect(...a: unknown[]) { this.rec('fillRect', a); }
  strokeRect(...a: unknown[]) { this.rec('strokeRect', a); }
  beginPath() { this.rec('beginPath', []); }
  closePath() { this.rec('closePath', []); }
  moveTo(...a: unknown[]) { this.rec('moveTo', a); }
  lineTo(...a: unknown[]) { this.rec('lineTo', a); }
  arcTo(...a: unknown[]) { this.rec('arcTo', a); }
  rect(...a: unknown[]) { this.rec('rect', a); }
  stroke() { this.rec('stroke', []); }
  fill() { this.rec('fill', []); }
  clip() { this.rec('clip', []); }
  fillText(...a: unknown[]) { this.rec('fillText', a); }
  setLineDash(d: number[]) { this.dash = [...d]; }
  getLineDash() { return this.dash; }
  save() { this.stack.push({ fillStyle: this.fillStyle, strokeStyle: this.strokeStyle, font: this.font, lineWidth: this.lineWidth, dash: this.dash }); this.rec('save', []); }
  restore() { const s = this.stack.pop(); if (s) Object.assign(this, { fillStyle: s.fillStyle, strokeStyle: s.strokeStyle, font: s.font, lineWidth: s.lineWidth, dash: s.dash }); this.rec('restore', []); }
  measureText(t: string) { return { width: t.length * 6.5 } as TextMetrics; }
  createLinearGradient() { return { addColorStop() {} } as unknown as CanvasGradient; }
}

export const asCtx = (f: FakeCtx) => f as unknown as CanvasRenderingContext2D;
