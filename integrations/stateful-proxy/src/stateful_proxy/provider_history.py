"""Provider-shaped, ordered history canonicalization."""
from dataclasses import dataclass
import hashlib,json
class UnsupportedHistory(ValueError): pass
class CanonicalHistory(list):
 def __init__(self,x=(),bootstrap_digest=None): super().__init__(x);self.bootstrap_digest=bootstrap_digest
@dataclass(frozen=True)
class RootMessageBinding:
 provider:str;issued_item_digest:str
 @classmethod
 def from_actual_response(c,p,r): return c(p,digest_prefix(canonical_response_items(p,r)))
def bad(w): raise UnsupportedHistory("unsupported "+w)
def s(v,w):
 if not isinstance(v,str):bad(w)
 return v
def h(v): return "sha256:"+hashlib.sha256(json.dumps(v,sort_keys=True,separators=(",",":"),ensure_ascii=True).encode()).hexdigest()
def text(v,w):
 if isinstance(v,str):return [{"type":"text","text":v}]
 if not isinstance(v,list):bad(w)
 o=[]
 for b in v:
  if not isinstance(b,dict) or b.get("type") not in {"text","input_text","output_text"}:bad(w)
  o.append({"type":"text","text":s(b.get("text"),w)})
 return o
def anth(v,w):
 if isinstance(v,str):return text(v,w)
 if not isinstance(v,list):bad(w)
 o=[]
 for b in v:
  if not isinstance(b,dict):bad(w)
  t=b.get("type")
  if t=="text":o+=text([b],w)
  elif t=="tool_use":o.append({"type":t,"call_id":s(b.get("id"),w),"name":s(b.get("name"),w),"arguments":b.get("input")})
  elif t=="tool_result":o.append({"type":t,"call_id":s(b.get("tool_use_id"),w),"result":text(b.get("content",""),w)})
  elif t in {"thinking","redacted_thinking"}:o.append({"type":t,"value":s(b.get("thinking",b.get("data")),w)})
  else:bad(w)
 return o
def resp(v,w):
 if not isinstance(v,dict):bad(w)
 t=v.get("type")
 if t=="message":return {"type":t,"role":v.get("role"),"content":text(v.get("content"),w)}
 if t=="function_call":
  x={"type":t,"call_id":s(v.get("call_id"),w),"name":s(v.get("name"),w),"arguments":s(v.get("arguments"),w)}
  if "namespace" in v:x["namespace"]=s(v["namespace"],w)
  return x
 if t=="function_call_output":return {"type":t,"call_id":s(v.get("call_id"),w),"result":text(v.get("output"),w)}
 if t=="compaction":return {"type":t,"encrypted_content_digest":h(s(v.get("encrypted_content"),w))}
 bad(w)
def chat(v,w):
 if not isinstance(v,dict) or v.get("role") not in {"system","developer","user","assistant","tool"}:bad(w)
 x={"type":"message","role":v["role"],"content":text(v.get("content",""),w) if v.get("content") is not None else []}
 if v["role"]=="tool":x["call_id"]=s(v.get("tool_call_id"),w)
 if "tool_calls" in v:
  x["tool_calls"]=[]
  for c in v["tool_calls"]:
   f=c.get("function") if isinstance(c,dict) else None
   if not isinstance(f,dict):bad(w)
   x["tool_calls"].append({"type":"function_call","call_id":s(c.get("id"),w),"name":s(f.get("name"),w),"arguments":s(f.get("arguments"),w)})
 return x
def canonical_request_history(provider,body):
 if not isinstance(body,dict):bad("request")
 out=[];boot=[]
 if provider.lower()=="anthropic":
  if not isinstance(body.get("messages"),list):bad("Anthropic messages")
  for m in body["messages"]:
   if not isinstance(m,dict) or m.get("role") not in {"user","assistant"}:bad("Anthropic message")
   out.append({"type":"message","role":m["role"],"content":anth(m.get("content"),"Anthropic content")})
  return CanonicalHistory(out,h({"system":anth(body["system"],"system")}) if "system" in body else None)
 src=body.get("input") if "input" in body else body.get("messages")
 if not isinstance(src,list):bad("request")
 for v in src:
  x=resp(v,"Responses item") if "input" in body else chat(v,"Chat message")
  (boot if x.get("role") in {"system","developer"} else out).append(x)
 if "instructions" in body:boot.append({"instructions":body["instructions"]})
 return CanonicalHistory(out,h(boot) if boot else None)
def canonical_response_items(provider,v):
 if isinstance(v,(bytes,str)):
  raw=v.decode() if isinstance(v,bytes) else v;v=[json.loads(x[5:]) for x in raw.splitlines() if x.startswith("data:") and x[5:].strip()!="[DONE]"]
 if isinstance(v,list) or isinstance(v,dict) and isinstance(v.get("events"),list):
  ev=v if isinstance(v,list) else v["events"]
  if provider.lower()=="anthropic":
   active={};out=[]
   for e in ev:
    if not isinstance(e,dict) or e.get("type") in {"message_start","message_delta","message_stop","ping"}:continue
    if e.get("type")=="content_block_start":active[e.get("index")]=dict(e.get("content_block") or {},_parts=[])
    elif e.get("type")=="content_block_delta":
     d=e.get("delta") or {};i=e.get("index");key={"text_delta":"text","thinking_delta":"thinking","input_json_delta":"partial_json"}.get(d.get("type"))
     if i not in active or not key:bad("Anthropic SSE delta")
     active[i]["_parts"].append(s(d.get(key),"Anthropic SSE delta"))
    elif e.get("type")=="content_block_stop":
     b=active.pop(e.get("index"),None)
     if b is None:bad("Anthropic SSE stop")
     parts="".join(b.pop("_parts"));t=b.get("type")
     if t in {"text","thinking"}:b[t]=parts
     elif t=="tool_use" and parts:b["input"]=json.loads(parts)
     out.extend(anth([b],"Anthropic SSE content"))
    else:bad("Anthropic SSE event")
   if active:bad("Anthropic SSE unfinished")
   return [{"type":"message","role":"assistant","content":out}]
  o=[resp(e["item"],"SSE item") for e in ev if isinstance(e,dict) and e.get("type")=="response.output_item.done"]
  if not o:bad("SSE")
  return o
 if not isinstance(v,dict):bad("response")
 if provider.lower()=="anthropic":return [{"type":"message","role":"assistant","content":anth(v.get("content"),"Anthropic response")}]
 if isinstance(v.get("output"),list):return [resp(x,"Responses output") for x in v["output"]]
 if isinstance(v.get("choices"),list):return [chat(x.get("message"),"Chat choice") for x in v["choices"] if isinstance(x,dict)]
 bad("response")
def digest_prefix(items):return h(list(items))
def find_exact_inherited_prefix(fork_items,registered_prefix):
 f,p=list(fork_items),list(registered_prefix)
 if len(f)<len(p) or f[:len(p)]!=p:return None
 return len(p) if all(x.get("type")=="message" and x.get("role")=="user" and all(b.get("type")=="text" for b in x.get("content",[]) if isinstance(b,dict)) for x in f[len(p):]) else None

# Capture-complete overrides. These retain every recognized business field or fail.
_old_anthropic = anth
_old_response_item = resp


def anth(value, where):
    items = _old_anthropic(value, where)
    if not isinstance(value, list):
        return items
    for source, item in zip(value, items):
        if source.get("type") == "tool_result" and "is_error" in source:
            if not isinstance(source["is_error"], bool):
                bad(where)
            item["is_error"] = source["is_error"]
        if source.get("type") == "thinking" and "signature" in source:
            item.pop("value", None)
            item["thinking"] = s(source.get("thinking"), where)
            item["signature"] = s(source["signature"], where)
    return items


def resp(value, where):
    if not isinstance(value, dict):
        bad(where)
    kind = value.get("type")
    if kind == "message" and "role" not in value:
        value = {**value, "role": "user"}
    if kind in {"reasoning", "compaction"}:
        if set(value) - {"type", "id", "status", "encrypted_content", "cache_control"}:
            bad(where)
        return {"type": kind, "encrypted_content_digest": h(s(value.get("encrypted_content"), where))}
    if kind == "custom_tool_call":
        if set(value) - {"type", "id", "status", "call_id", "name", "arguments", "input", "namespace", "cache_control"}:
            bad(where)
        item = {"type": kind, "call_id": s(value.get("call_id"), where), "name": s(value.get("name"), where), "arguments": _json(value.get("arguments", value.get("input")), where)}
        if "namespace" in value:
            item["namespace"] = s(value["namespace"], where)
        return item
    if kind == "custom_tool_call_output":
        if set(value) - {"type", "id", "status", "call_id", "output", "cache_control"}:
            bad(where)
        return {"type": kind, "call_id": s(value.get("call_id"), where), "result": _json(value.get("output"), where)}
    return _old_response_item(value, where)


def _chat_sse(events):
    choices = {}
    for event in events:
        for choice in event.get("choices", []) if isinstance(event, dict) else []:
            if not isinstance(choice, dict) or not isinstance(choice.get("index", 0), int):
                bad("Chat SSE choice")
            state = choices.setdefault(choice.get("index", 0), {"text": [], "calls": {}})
            delta = choice.get("delta", {})
            if not isinstance(delta, dict):
                bad("Chat SSE delta")
            if isinstance(delta.get("content"), str):
                state["text"].append(delta["content"])
            for raw in delta.get("tool_calls", []):
                if not isinstance(raw, dict) or not isinstance(raw.get("index"), int):
                    bad("Chat SSE tool call")
                call = state["calls"].setdefault(raw["index"], {"id": None, "name": None, "arguments": []})
                if "id" in raw:
                    call["id"] = raw["id"]
                function = raw.get("function")
                if function is not None:
                    if not isinstance(function, dict):
                        bad("Chat SSE function")
                    if "name" in function:
                        call["name"] = function["name"]
                    if isinstance(function.get("arguments"), str):
                        call["arguments"].append(function["arguments"])
    if not choices:
        bad("Chat SSE")
    result = []
    for _, state in sorted(choices.items()):
        item = {"type": "message", "role": "assistant", "content": text("".join(state["text"]), "Chat SSE text")}
        if state["calls"]:
            item["tool_calls"] = [{"type": "function_call", "call_id": s(call["id"], "Chat SSE id"), "name": s(call["name"], "Chat SSE name"), "arguments": "".join(call["arguments"])} for _, call in sorted(state["calls"].items())]
        result.append(item)
    return result


def canonical_response_items(provider, response):
    events = _events(response)
    if events is not None:
        if any(isinstance(event, dict) and isinstance(event.get("choices"), list) for event in events):
            return _chat_sse(events)
        if provider.lower() != "anthropic":
            items = [resp(event["item"], "Responses SSE item") for event in events if isinstance(event, dict) and event.get("type") == "response.output_item.done"]
            if not items:
                bad("Responses SSE")
            return items
    return _old_canonical_response_items(provider, response)


_old_canonical_response_items = globals().get("canonical_response_items")


@dataclass(frozen=True)
class RootMessageBinding:
    provider: str
    issued_item_digest: str
    bootstrap_digest: str | None = None

    @classmethod
    def from_actual_response(cls, provider, response, bootstrap_digest=None):
        return cls(provider, digest_prefix(canonical_response_items(provider, response)), bootstrap_digest)


def _json(value, where):
    try:
        return json.loads(json.dumps(value, sort_keys=True, ensure_ascii=True, allow_nan=False))
    except (TypeError, ValueError) as error:
        raise _bad(where) from error


def _events(value):
    if isinstance(value, dict) and isinstance(value.get("events"), list):
        return value["events"]
    if isinstance(value, list):
        return value
    if isinstance(value, bytes):
        value = value.decode("utf-8")
    if not isinstance(value, str) or "data:" not in value:
        return None
    events = []
    for group in value.replace("\r\n", "\n").split("\n\n"):
        data = [line[5:].strip() for line in group.splitlines() if line.startswith("data:")]
        if data and data != ["[DONE]"]:
            events.append(json.loads("\n".join(data)))
    return events


def _anthropic_sse(events):
    active, output = {}, []
    for event in events:
        if not isinstance(event, dict): bad("Anthropic SSE event")
        kind = event.get("type")
        if kind in {"message_start", "message_delta", "message_stop", "ping"}:
            continue
        if kind == "content_block_start":
            index = event.get("index")
            if not isinstance(index, int) or not isinstance(event.get("content_block"), dict): bad("Anthropic SSE start")
            active[index] = {**event["content_block"], "_parts": []}
        elif kind == "content_block_delta":
            index, delta = event.get("index"), event.get("delta")
            if index not in active or not isinstance(delta, dict): bad("Anthropic SSE delta")
            if delta.get("type") == "signature_delta": active[index]["signature"] = s(delta.get("signature"), "Anthropic SSE signature")
            else:
                key = {"text_delta": "text", "thinking_delta": "thinking", "input_json_delta": "partial_json"}.get(delta.get("type"))
                if not key: bad("Anthropic SSE delta")
                active[index]["_parts"].append(s(delta.get(key), "Anthropic SSE delta"))
        elif kind == "content_block_stop":
            block = active.pop(event.get("index"), None)
            if block is None: bad("Anthropic SSE stop")
            value = "".join(block.pop("_parts"))
            if block.get("type") in {"text", "thinking"}: block[block["type"]] = value
            elif block.get("type") == "tool_use" and value: block["input"] = json.loads(value)
            output.extend(anth([block], "Anthropic SSE content"))
        else: bad("Anthropic SSE event")
    if active: bad("Anthropic SSE unfinished")
    return [{"type": "message", "role": "assistant", "content": output}]


def canonical_response_items(provider, response):
    events = _events(response)
    if events is not None:
        if provider.lower() == "anthropic": return _anthropic_sse(events)
        if any(isinstance(event, dict) and isinstance(event.get("choices"), list) for event in events): return _chat_sse(events)
        items = [resp(event["item"], "Responses SSE item") for event in events if isinstance(event, dict) and event.get("type") == "response.output_item.done"]
        if not items: bad("Responses SSE")
        return items
    if not isinstance(response, dict): bad("response")
    if provider.lower() == "anthropic": return [{"type": "message", "role": "assistant", "content": anth(response.get("content"), "Anthropic response")}]
    if isinstance(response.get("output"), list): return [resp(item, "Responses output") for item in response["output"]]
    if isinstance(response.get("choices"), list): return [chat(choice.get("message"), "Chat response") for choice in response["choices"] if isinstance(choice, dict)]
    bad("response")


def resp(value, where):
    """Responses items observed in baseline and review-relay captures."""
    if not isinstance(value, dict):
        bad(where)
    kind = value.get("type")
    transport = {"id", "cache_control"}
    if kind == "message":
        allowed = {"type", "role", "content", "phase", "status"} | transport
        if set(value) - allowed:
            bad(where)
        role = value.get("role", "user")
        if role not in {"user", "assistant", "system", "developer"}:
            bad(where)
        item = {"type": kind, "role": role, "content": text(value.get("content"), where)}
        if "phase" in value:
            item["phase"] = _json(value["phase"], where)
        return item
    if kind == "reasoning":
        allowed = {"type", "encrypted_content", "summary", "content"} | transport
        if set(value) - allowed or "encrypted_content" not in value:
            bad(where)
        item = {"type": kind, "encrypted_content_digest": h(s(value["encrypted_content"], where))}
        for field in ("summary", "content"):
            if field not in value or value[field] in (None, []):
                continue
            item[field] = _json(value[field], where)
        return item
    if kind == "function_call":
        allowed = {"type", "call_id", "name", "arguments", "namespace", "status"} | transport
        if set(value) - allowed:
            bad(where)
        item = {"type": kind, "call_id": s(value.get("call_id"), where), "name": s(value.get("name"), where), "arguments": s(value.get("arguments"), where)}
        if "namespace" in value:
            if value["namespace"] is not None and not isinstance(value["namespace"], str):
                bad(where)
            item["namespace"] = value["namespace"]
        if "status" in value:
            item["status"] = _json(value["status"], where)
        return item
    if kind == "function_call_output":
        if set(value) - ({"type", "call_id", "output"} | transport):
            bad(where)
        return {"type": kind, "call_id": s(value.get("call_id"), where), "result": text(value.get("output"), where)}
    if kind == "custom_tool_call":
        allowed = {"type", "call_id", "name", "arguments", "input", "namespace", "status"} | transport
        if set(value) - allowed: bad(where)
        item = {"type": kind, "call_id": s(value.get("call_id"), where), "name": s(value.get("name"), where), "arguments": _json(value.get("arguments", value.get("input")), where)}
        if "namespace" in value: item["namespace"] = value["namespace"]
        return item
    if kind == "custom_tool_call_output":
        if set(value) - ({"type", "call_id", "output", "status"} | transport): bad(where)
        return {"type": kind, "call_id": s(value.get("call_id"), where), "result": _json(value.get("output"), where)}
    if kind in {"tool_search_call", "tool_search_output"}:
        allowed = {"type", "call_id", "arguments", "execution", "tools", "status"} | transport
        if set(value) - allowed:
            bad(where)
        item = {"type": kind, "call_id": s(value.get("call_id"), where)}
        for field in ("arguments", "execution", "tools", "status"):
            if field in value:
                item[field] = _json(value[field], where)
        return item
    return _old_response_item(value, where)
