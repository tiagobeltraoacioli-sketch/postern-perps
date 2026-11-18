// Phase E: liquidation through the book, through the backstop, by ADL. Not built yet: recorded as blocked, never as passed. UNAUDITED prototype.
export default async function (ctx) {
  await ctx.blocked('E-01', "liquidation through the book, through the backstop, by ADL", "needs a scripted oracle price path (432's replay feeder through 431's ingest); main has only the fixed-price dev oracle");
}
