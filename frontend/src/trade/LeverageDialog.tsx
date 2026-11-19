/** Leverage and margin mode (UpdateLeverage) and isolated margin (UpdateMargin): both through the review sheet. */
import { useState } from 'react';
import { Modal } from '../ui/Modal';
import { useActions } from '../state/actions';
import type { Market, Position } from '../lib/types';
import { formatUsd } from '../lib/format';
import { USDC_DECIMALS } from '../lib/fixed';
import { parseAt } from './order-builder';

export function LeverageDialog({ market, current, isolated, onClose }: { market: Market; current: number; isolated: boolean; onClose: () => void }) {
  const { request } = useActions();
  const [lev, setLev] = useState(current);
  const [iso, setIso] = useState(isolated);
  const tierCaps = market.tiers.map((t, i) => `${t.maxLeverage}x up to ${i + 1 < market.tiers.length ? `${formatUsd(market.tiers[i + 1].floor, { dp: 0 })} USDC` : 'any size'}`);
  return (
    <Modal title={`${market.symbol} leverage`} onClose={onClose} eyebrow="Margin settings">
      <div className="dialog-body">
        <label className="field">
          <span>
            Leverage <strong className="num">{lev}x</strong>
          </span>
          <input type="range" min={1} max={market.maxLeverage} step={1} value={lev} onChange={(e) => setLev(Number(e.target.value))} aria-valuetext={`${lev} times`} />
        </label>
        <p className="fine">Margin tiers: {tierCaps.join('; ')}. A larger position allows less leverage.</p>
        <fieldset className="radio-cards">
          <legend>Margin mode</legend>
          <label className={!iso ? 'selected' : ''}>
            <input type="radio" name="mode" checked={!iso} onChange={() => setIso(false)} />
            <strong>Cross</strong>
            <span>The whole account balance backs this position. A loss here can use margin of other positions.</span>
          </label>
          <label className={iso ? 'selected' : ''}>
            <input type="radio" name="mode" checked={iso} onChange={() => setIso(true)} />
            <strong>Isolated</strong>
            <span>Only the margin put into this position is at risk; it is liquidated sooner.</span>
          </label>
        </fieldset>
        <p className="callout callout-warn">Higher leverage moves the liquidation price closer to the market price.</p>
        <div className="review-actions">
          <button type="button" className="button button-quiet" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="button button-primary"
            onClick={() => {
              onClose();
              request('UpdateLeverage', (nonce, account) => ({ account, nonce: nonce.toString(), marketId: market.marketId, leverage: lev, isolated: iso }));
            }}
          >
            Review
          </button>
        </div>
      </div>
    </Modal>
  );
}

export function MarginDialog({ market, position, onClose }: { market: Market; position: Position; onClose: () => void }) {
  const { request } = useActions();
  const [add, setAdd] = useState(true);
  const [amount, setAmount] = useState('');
  const parsed = parseAt(amount, USDC_DECIMALS, USDC_DECIMALS);
  const value = parsed.value;
  return (
    <Modal title={`${market.symbol} isolated margin`} onClose={onClose} eyebrow="Margin settings">
      <div className="dialog-body">
        <p>
          Margin in this position: <strong className="num">{formatUsd(position.isolatedMargin)} USDC</strong>
        </p>
        <div className="segmented" role="group" aria-label="Direction">
          <button type="button" aria-pressed={add} onClick={() => setAdd(true)}>
            Add
          </button>
          <button type="button" aria-pressed={!add} onClick={() => setAdd(false)}>
            Remove
          </button>
        </div>
        <label className="field">
          <span>Amount (USDC)</span>
          <input className="num" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value.replace(/[^\d.]/g, ''))} />
        </label>
        {parsed.error && <p className="msg-danger">Amount {parsed.error}.</p>}
        {!add && <p className="callout callout-warn">Removing margin moves the liquidation price closer to the market price.</p>}
        <div className="review-actions">
          <button type="button" className="button button-quiet" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="button button-primary"
            disabled={!value}
            onClick={() => {
              onClose();
              request('UpdateMargin', (nonce, account) => ({ account, nonce: nonce.toString(), marketId: market.marketId, amount: value!.toString(), add }));
            }}
          >
            Review
          </button>
        </div>
      </div>
    </Modal>
  );
}
