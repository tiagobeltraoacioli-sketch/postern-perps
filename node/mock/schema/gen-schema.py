import json,sys
R=lambda n:{"$ref":"#/$defs/"+n}
def obj(props,req=None,opt=()):
    # props: dict name->schema; all required unless in opt
    req=[k for k in props if k not in opt] if req is None else req
    return {"type":"object","properties":props,"required":req,"additionalProperties":False}
def arr(s,**kw): d={"type":"array","items":s}; d.update(kw); return d
def nul(s): return {"anyOf":[s,{"type":"null"}]}
def enum(*v): return {"type":"string","enum":list(v)}
D={}
D["Dec"]={"type":"string","pattern":r"^-?(0|[1-9][0-9]*)(\.[0-9]+)?$","description":"decimal string in human units (API.md 1)"}
D["UDec"]={"type":"string","pattern":r"^(0|[1-9][0-9]*)(\.[0-9]+)?$","description":"non-negative decimal string"}
D["U64"]={"type":"string","pattern":r"^(0|[1-9][0-9]{0,19})$","description":"64-bit identifier or nonce as a decimal string"}
D["Int"]={"type":"integer","minimum":0,"maximum":9007199254740991}
D["Ms"]={"type":"integer","minimum":0,"maximum":9007199254740991,"description":"Unix milliseconds"}
D["Address"]={"type":"string","pattern":r"^0x[0-9a-f]{40}$","description":"lower-case 0x address"}
D["Hash"]={"type":"string","pattern":r"^0x[0-9a-f]{64}$"}
D["Hex"]={"type":"string","pattern":r"^0x([0-9a-f]{2})*$"}
D["Sig"]={"type":"string","pattern":r"^0x[0-9a-f]{130}$","description":"65 bytes r||s||v"}
D["Symbol"]={"type":"string","pattern":r"^[A-Z0-9]{2,12}-USD$"}
D["Side"]=enum("buy","sell")
D["Error"]=obj({"error":obj({"code":{"type":"string","pattern":r"^[A-Z][A-Z0-9_]*$"},"message":{"type":"string"},"details":{"type":"object"}})})
D["MarginTier"]=obj({"notionalFloor":R("UDec"),"initialMarginFraction":R("UDec"),"maintenanceMarginFraction":R("UDec"),"maxLeverage":R("UDec")})
D["OracleConfig"]=obj({"sources":arr({"type":"string","minLength":1},minItems=1),"minSources":R("Int"),"maxStalenessMs":R("Int"),"maxDeviationBps":R("UDec")})
D["Market"]=obj({"symbol":R("Symbol"),"marketId":{"type":"integer","minimum":1,"maximum":4294967295},"base":{"type":"string","pattern":"^[A-Z0-9]{2,12}$"},"quote":{"const":"USD"},
 "status":enum("active","halted","closeOnly","settled"),"priceDecimals":{"type":"integer","minimum":0,"maximum":18},"sizeDecimals":{"type":"integer","minimum":0,"maximum":18},
 "tickSize":R("UDec"),"stepSize":R("UDec"),"minOrderSize":R("UDec"),"maxOrderSize":R("UDec"),"maxLeverage":R("UDec"),"marginTiers":arr(R("MarginTier"),minItems=1),
 "makerFeeBps":R("Dec"),"takerFeeBps":R("UDec"),"liquidationFeeBps":R("UDec"),"fundingIntervalSec":R("Int"),"maxFundingRate":R("UDec"),"oracle":R("OracleConfig"),"openInterestCap":R("UDec"),
 "haltReason":nul({"type":"string"})},opt=("haltReason",))
D["Ticker"]=obj({"symbol":R("Symbol"),"markPrice":R("UDec"),"indexPrice":R("UDec"),"lastPrice":R("UDec"),"fundingRate":R("Dec"),"nextFundingTime":R("Ms"),"openInterest":R("UDec"),
 "volume24h":R("UDec"),"high24h":R("UDec"),"low24h":R("UDec"),"price24hAgo":R("UDec"),"change24h":R("Dec"),"height":R("Int"),"time":R("Ms")})
D["Level"]={"type":"array","prefixItems":[R("UDec"),R("UDec")],"minItems":2,"maxItems":2}
D["Book"]=obj({"symbol":R("Symbol"),"seq":R("Int"),"height":R("Int"),"time":R("Ms"),"bids":arr(R("Level")),"asks":arr(R("Level"))})
D["BookDelta"]=obj({"symbol":R("Symbol"),"seq":R("Int"),"prevSeq":R("Int"),"height":R("Int"),"time":R("Ms"),"bids":arr(R("Level")),"asks":arr(R("Level"))})
D["Trade"]=obj({"tradeId":R("U64"),"symbol":R("Symbol"),"price":R("UDec"),"size":R("UDec"),"side":R("Side"),"time":R("Ms"),"height":R("Int"),"liquidation":{"type":"boolean"}})
D["Candle"]=obj({"t":R("Ms"),"o":R("UDec"),"h":R("UDec"),"l":R("UDec"),"c":R("UDec"),"v":R("UDec"),"q":R("UDec"),"n":R("Int")})
D["FundingRate"]=obj({"symbol":R("Symbol"),"time":R("Ms"),"rate":R("Dec"),"premium":R("Dec"),"markPrice":R("UDec"),"indexPrice":R("UDec")})
D["OracleSource"]=obj({"name":{"type":"string"},"price":nul(R("UDec")),"time":nul(R("Ms")),"used":{"type":"boolean"},"reason":{"type":["string","null"]}},opt=("reason",))
D["OracleStatus"]=obj({"symbol":R("Symbol"),"status":enum("ok","stale","insufficientSources","deviation","halted"),"price":R("UDec"),"time":R("Ms"),"round":R("U64"),
 "sources":arr(R("OracleSource")),"signers":R("Int"),"threshold":R("Int"),"haltReason":{"type":["string","null"]}})
D["BlockRef"]=obj({"height":R("Int"),"time":R("Ms"),"hash":R("Hash"),"stateRoot":R("Hash"),"logHash":R("Hash")})
D["Status"]=obj({"name":{"type":"string"},"version":{"type":"string"},"apiVersion":{"type":"string","pattern":r"^[0-9]+\.[0-9]+$"},"mode":enum("mock","dev","live"),"unaudited":{"type":"boolean"},
 "network":obj({"eip712":R("Domain"),"collateralChainId":{"const":700770},"vault":R("Address"),"collateralToken":R("Address"),"collateralSymbol":{"type":"string"},"collateralDecimals":{"const":6}}),
 "time":R("Ms"),"latestBlock":R("BlockRef"),"latestFinalized":nul(R("BlockRef")),
 "sequencer":obj({"state":enum("running","recovering","halted"),"blockIntervalMs":R("Int"),"pendingActions":R("Int")}),
 "committee":obj({"n":R("Int"),"threshold":R("Int"),"online":R("Int")}),"vaultMode":enum("normal","paused","escape","unknown"),
 "l2":obj({"finalizedBlock":R("Int"),"lastDepositBlock":R("Int"),"endpointsAgree":{"type":"boolean"}})})
D["CheckpointSig"]=obj({"validator":R("Address"),"signature":R("Sig")})
D["Checkpoint"]=obj({"height":R("Int"),"time":R("Ms"),"blockHash":R("Hash"),"stateRoot":R("Hash"),"logHash":R("Hash"),
 "accountsRoot":R("Hash"),"marketsRoot":R("Hash"),"withdrawalsRoot":R("Hash"),"nextDepositIndex":R("Int"),"nextForcedIndex":R("Int"),"nextOrderId":R("U64"),
 "totalEscape":{"type":"string","pattern":"^(0|[1-9][0-9]{0,38})$"},"dust":{"type":"string","pattern":"^(0|[1-9][0-9]{0,38})$"},"auxHash":R("Hash"),
 "depositsHead":R("Hash"),"forcedHead":R("Hash"),"withdrawalCount":R("Int"),"withdrawalsTotal":R("U64"),
 "digest":R("Hash"),"threshold":R("Int"),"final":{"type":"boolean"},"signatures":arr(R("CheckpointSig"))})
D["Committee"]=obj({"threshold":R("Int"),"validators":arr(R("Address"),minItems=1),"setHash":R("Hash")})
D["BlockInput"]={"oneOf":[
 obj({"kind":{"const":"action"},"account":R("Address"),"signer":R("Address"),"type":{"type":"string"},"message":{"type":"object"},"signature":R("Sig")},opt=("signer",)),
 obj({"kind":{"enum":["oracle","deposit","forced"]},"payload":{"type":"object"}})]}
D["Block"]=obj({"height":R("Int"),"time":R("Ms"),"prevLogHash":R("Hash"),"logHash":R("Hash"),"stateRoot":R("Hash"),"inputs":arr(R("BlockInput")),"inputsEncoded":R("Hex"),"sequencerSignature":R("Sig")})
D["FeeTier"]=obj({"tier":R("Int"),"makerFeeBps":R("Dec"),"takerFeeBps":R("UDec"),"volume30d":R("UDec")})
D["Position"]=obj({"symbol":R("Symbol"),"size":R("Dec"),"entryPrice":R("UDec"),"markPrice":R("UDec"),"notional":R("UDec"),"unrealizedPnl":R("Dec"),"realizedPnl":R("Dec"),
 "liquidationPrice":nul(R("UDec")),"marginMode":enum("cross","isolated"),"leverage":R("UDec"),"isolatedMargin":nul(R("UDec")),"initialMargin":R("UDec"),"maintenanceMargin":R("UDec"),"fundingSinceOpen":R("Dec")})
D["Account"]=obj({"address":R("Address"),"height":R("Int"),"time":R("Ms"),"collateral":R("Dec"),"unrealizedPnl":R("Dec"),"equity":R("Dec"),"initialMargin":R("UDec"),"maintenanceMargin":R("UDec"),
 "freeCollateral":R("Dec"),"withdrawable":R("UDec"),"marginRatio":R("UDec"),"feeTier":R("FeeTier"),"positions":arr(R("Position")),"openOrders":R("Int")})
D["Order"]=obj({"orderId":R("U64"),"clientOrderId":nul(R("Hash")),"account":R("Address"),"symbol":R("Symbol"),"side":R("Side"),
 "type":enum("limit","market","stopMarket","stopLimit","takeProfitMarket","takeProfitLimit"),"timeInForce":enum("GTC","IOC","POST_ONLY"),"price":R("UDec"),"size":R("UDec"),"filledSize":R("UDec"),
 "avgFillPrice":nul(R("UDec")),"reduceOnly":{"type":"boolean"},"triggerPrice":nul(R("UDec")),"triggerCondition":{"enum":["markAbove","markBelow",None]},
 "status":enum("untriggered","open","filled","canceled","rejected","expired"),"createdAt":R("Ms"),"updatedAt":R("Ms"),"height":R("Int"),"rejectReason":{"type":["string","null"]}})
D["Fill"]=obj({"fillId":R("U64"),"tradeId":R("U64"),"orderId":R("U64"),"account":R("Address"),"symbol":R("Symbol"),"side":R("Side"),"price":R("UDec"),"size":R("UDec"),"fee":R("Dec"),
 "liquidity":enum("maker","taker"),"realizedPnl":R("Dec"),"liquidation":{"type":"boolean"},"time":R("Ms"),"height":R("Int")})
D["FundingPayment"]=obj({"symbol":R("Symbol"),"time":R("Ms"),"rate":R("Dec"),"positionSize":R("Dec"),"markPrice":R("UDec"),"payment":R("Dec")})
D["Agent"]=obj({"agent":R("Address"),"name":{"type":"string","maxLength":64},"approvedAt":R("Ms"),"expiresAt":R("Ms"),"height":R("Int")})
# API 0.3 (431 b897344): leafData = the vault's Account struct fields (VAULT.md claimEscape) + the 88 leaf bytes; the proof is
# exactly the 32 siblings of the depth-32 account tree. A looser shape here would let a node serve a proof the vault refuses.
D["SDecInt"]={"type":"string","pattern":r"^-?(0|[1-9][0-9]*)$","description":"signed integer as a decimal string (USDC units)"}
D["AccountLeaf"]=obj({"account":R("Address"),"index":R("Int"),"balance":R("SDecInt"),"escapeValue":R("U64"),"positionsHash":R("Hash"),"encoded":{"type":"string","pattern":r"^0x[0-9a-f]{176}$"}})
D["AccountProof"]=obj({"address":R("Address"),"checkpoint":R("Checkpoint"),"leaf":R("Hash"),"leafData":R("AccountLeaf"),"proof":arr(R("Hash"),minItems=32,maxItems=32),"index":R("U64")})
D["WithdrawalRecord"]={"type":"object"}
D["WithdrawalProof"]=obj({"withdrawalId":R("U64"),"count":R("U64"),"checkpointHeight":nul(R("Int")),"withdrawalsRoot":R("Hash"),"record":R("WithdrawalRecord"),"leaf":R("Hash"),"siblings":arr(R("Hash"),minItems=32,maxItems=32)})
D["OracleIngestAck"]={"type":"object","properties":{"round":R("U64"),"signer":R("Int"),"held":R("Int"),"quorum":R("Int"),"status":enum("held","ready"),"observations":arr({"type":"object"})},"required":["round","held","quorum","status"],"additionalProperties":False}
D["OracleRoundSigners"]=obj({"round":R("U64"),"signers":arr(R("Int"))})
D["OracleRounds"]=obj({"enabled":{"type":"boolean"},"quorum":R("Int"),"signers":arr(R("Address")),"gatherMs":R("Int"),"accepted":R("Int"),"refused":R("Int"),"conflicts":R("Int"),"roundsBuilt":R("Int"),
 "lastBuilt":nul(R("OracleRoundSigners")),"lastAcceptedRound":nul(R("U64")),"held":arr(obj({"round":R("U64"),"signers":arr(R("Int")),"quorumAtMs":nul(R("Ms"))}))},opt=("lastBuilt","lastAcceptedRound"))
D["Deposit"]=obj({"depositId":R("U64"),"account":R("Address"),"amount":R("UDec"),"l2TxHash":R("Hash"),"logIndex":R("Int"),"l2Block":R("Int"),"status":enum("seen","l2Finalized","credited"),"creditedHeight":nul(R("Int")),"time":R("Ms")})
D["WithdrawalCertificate"]=obj({"digest":R("Hash"),"record":{"type":"object"},"checkpoint":R("Checkpoint"),"proof":arr(R("Hash")),"signatures":arr(R("CheckpointSig"))})
D["Withdrawal"]=obj({"withdrawalId":R("U64"),"account":R("Address"),"recipient":R("Address"),"amount":R("UDec"),"fee":R("UDec"),
 "status":enum("requested","included","finalized","certified","submitted","delayed","paid","rejected"),"requestedHeight":R("Int"),"checkpointHeight":nul(R("Int")),
 "certificate":nul(R("WithdrawalCertificate")),"l2TxHash":nul(R("Hash")),"availableAt":nul(R("Ms")),"time":R("Ms")})
D["ForcedAction"]=obj({"forcedId":R("U64"),"account":R("Address"),"kind":enum("cancelAll","close","withdraw"),"l2TxHash":R("Hash"),"l2Block":R("Int"),"deadlineTime":R("Ms"),"status":enum("queued","included","expired"),"includedHeight":nul(R("Int")),
 # API 0.3 additions (optional: 0.2 servers omit them): the engine's verdict on the request (SPEC 14)
 "arg":R("U64"),"outcome":{"enum":["served","rejected","pending",None]},"reason":{"type":["string","null"]},"processedHeight":nul(R("Int"))},opt=("arg","outcome","reason","processedHeight"))
D["TypedField"]=obj({"name":{"type":"string","pattern":"^[a-zA-Z][a-zA-Z0-9]*$"},"type":{"type":"string","pattern":r"^(address|bool|string|bytes32|u?int(8|16|32|64|128|256))$"}})
D["Domain"]=obj({"name":{"const":"PosternPerps"},"version":{"const":"1"},"chainId":{"const":700770},"verifyingContract":R("Address")})
D["Eip712"]={"type":"object","properties":{"domain":R("Domain"),"scales":obj({"price":{"const":8},"size":{"const":8},"usdc":{"const":6}}),
 "types":{"type":"object","additionalProperties":arr(R("TypedField"),minItems=2)},"actions":arr({"type":"string"},minItems=1),"agentAllowed":arr({"type":"string"}),"specVersion":{"type":"string"},
 "enums":{"type":"object"},"notes":{"type":"object"},"_note":{"type":"string"}},"required":["domain","scales","types","actions","agentAllowed","specVersion"],"additionalProperties":False}
D["ActionResult"]=obj({"ok":{"type":"boolean"},"orderId":nul(R("U64")),"orderStatus":{"type":["string","null"]},"filledSize":nul(R("UDec")),"withdrawalId":nul(R("U64")),"error":{"type":["string","null"]}},opt=("orderId","orderStatus","filledSize","withdrawalId"))
D["ActionAck"]=obj({"actionHash":R("Hash"),"status":enum("accepted","included","rejected"),"receivedAt":R("Ms"),"height":R("Int"),"index":R("Int"),"result":R("ActionResult")},opt=("height","index","result"))
D["GeoStatus"]=obj({"country":{"type":["string","null"],"pattern":"^[A-Z0-9]{2}$"},"restricted":{"type":"boolean"},"policy":enum("actions_refused","exit_only","open"),"listVersion":{"type":"string"},
 "reasonCode":{"type":["string","null"]},"message":{"type":["string","null"]},"sanctioned":{"type":"boolean"}})
D["NonceInfo"]=obj({"account":R("Address"),"nextNonce":R("U64"),"agents":arr(obj({"agent":R("Address"),"nextNonce":R("U64")}))})
cur=lambda: {"type":"string","minLength":1}
def lst(key,item,cursor=True):
    p={key:arr(R(item))}
    if cursor: p["nextCursor"]=cur()
    return obj(p,opt=("nextCursor",))
D["MarketList"]=lst("markets","Market",False); D["TickerList"]=lst("tickers","Ticker",False)
D["TradeList"]=lst("trades","Trade"); D["CandleList"]=lst("candles","Candle",False); D["FundingRateList"]=lst("funding","FundingRate")
D["BlockList"]=lst("blocks","Block",False); D["OrderList"]=lst("orders","Order"); D["FillList"]=lst("fills","Fill"); D["FundingPaymentList"]=lst("payments","FundingPayment")
D["DepositList"]=lst("deposits","Deposit",False); D["WithdrawalList"]=lst("withdrawals","Withdrawal",False); D["AgentList"]=lst("agents","Agent",False); D["ForcedList"]=lst("forced","ForcedAction",False)
D["AccountSnapshot"]=obj({"account":R("Account"),"orders":arr(R("Order")),"agents":arr(R("Agent"))})
D["AccountUpdate"]=obj({"height":R("Int"),"time":R("Ms"),"account":R("Account"),"orders":arr(R("Order")),"fills":arr(R("Fill")),"funding":arr(R("FundingPayment")),"deposits":arr(R("Deposit")),"withdrawals":arr(R("Withdrawal")),"actions":arr(R("ActionAck"))},req=["height","time"])
D["StatusUpdate"]=obj({"latestBlock":R("BlockRef"),"latestFinalized":nul(R("BlockRef"))})
def ws(channel,typ,data,key=None):
    p={"channel":{"const":channel},"type":{"const":typ},"data":data}
    if key: p[key]= R("Address") if key=="account" else ({"type":"string","enum":["1m","5m","15m","1h","4h","1d"]} if key=="interval" else R("Symbol"))
    return p
W=[]
W.append(obj({**ws("book","snapshot",R("Book"),"symbol")})); W.append(obj({**ws("book","delta",R("BookDelta"),"symbol")}))
W.append(obj({**ws("trades","snapshot",obj({"trades":arr(R("Trade"))}),"symbol")})); W.append(obj({**ws("trades","update",obj({"trades":arr(R("Trade"))}),"symbol")}))
W.append(obj({**ws("ticker","update",R("Ticker"),"symbol")}))
c=ws("candles","snapshot",obj({"candles":arr(R("Candle"))}),"symbol"); c["interval"]={"enum":["1m","5m","15m","1h","4h","1d"]}; W.append(obj(c))
c=ws("candles","update",R("Candle"),"symbol"); c["interval"]={"enum":["1m","5m","15m","1h","4h","1d"]}; W.append(obj(c))
W.append(obj({**ws("account","snapshot",R("AccountSnapshot"),"account")})); W.append(obj({**ws("account","update",R("AccountUpdate"),"account")}))
W.append(obj({**ws("status","update",R("StatusUpdate"))}))
D["WsData"]={"oneOf":W}
chan={"enum":["book","trades","ticker","candles","account","status"]}
D["WsControl"]={"oneOf":[
 obj({"op":{"enum":["subscribed","unsubscribed"]},"id":{"type":["integer","string"]},"channel":chan,"symbol":{"type":"string"},"interval":{"type":"string"},"account":R("Address")},opt=("id","symbol","interval","account")),
 obj({"op":{"const":"actionResult"},"id":{"type":["integer","string"]},"ack":R("ActionAck")},opt=("id",)),
 obj({"op":{"const":"error"},"id":{"type":["integer","string","null"]},"error":D["Error"]["properties"]["error"]},opt=("id",)),
 obj({"op":{"const":"pong"},"time":R("Ms")})]}
D["WsMessage"]={"anyOf":[R("WsData"),R("WsControl")]}
D["ActionRequest"]={"type":"object","properties":{"type":{"type":"string","pattern":"^[A-Z][A-Za-z]+$"},"message":{"type":"object"},"signature":R("Sig"),"wire":{"type":"boolean"}},"required":["type","message","signature"],"additionalProperties":False}
schema={"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"postern-perps/api/0.1","title":"PosternDEX Perpetuals API v0.1 (UNAUDITED prototype)",
 "description":"Draft by agent 393 from docs/API.md v0.1 (2811600) and docs/SPEC.md v0.1 (014c466: domain 700770 + verifyingContract, fixed scales) for node/api/schema.json (owner 389). Checkpoint carries the sixteen fields of the VAULT.md v0.2 digest (as 389's node b170e74 serialises them) so anyone can recompute the digest and the SPEC 11 state-root equation. Response objects are closed (additionalProperties false): an added field is a schema change first, so the node, the mock and the frontend cannot drift silently.",
 "$defs":D}
json.dump(schema,open(sys.argv[1],'w'),indent=1)
