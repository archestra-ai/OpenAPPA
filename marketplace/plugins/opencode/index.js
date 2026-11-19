// @bun
var __defProp = Object.defineProperty;
var __returnValue = (v) => v;
function __exportSetter(name, newValue) {
  this[name] = __returnValue.bind(null, newValue);
}
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, {
      get: all[name],
      enumerable: !0,
      configurable: !0,
      set: __exportSetter.bind(all, name)
    });
};

// integrations/opencode/appencode.ts
import { Plugin } from "@opencode/plugin/effect";

// integrations/opencode/node_modules/@opencode/schema/dist/agent.js
var exports_agent = {};
__export(exports_agent, {
  Name: () => Name,
  Info: () => Info7,
  ID: () => ID8,
  Event: () => Event7,
  Color: () => Color,
  Agent: () => exports_agent
});
import { Schema as Schema17 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/event.js
import { Schema as Schema5, SchemaTransformation } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/schema.js
import { DateTime, Option, Schema, SchemaGetter } from "effect";
var PositiveInt = Schema.Int.check(Schema.isGreaterThan(0)), NonNegativeInt = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)), RelativePath = Schema.String.pipe(Schema.brand("RelativePath"), Schema.annotate({ identifier: "RelativePath" })), AbsolutePath = Schema.String.pipe(Schema.brand("AbsolutePath"), Schema.annotate({ identifier: "AbsolutePath" })), optional = (schema) => Schema.optionalKey(schema).pipe(Schema.decodeTo(Schema.optional(Schema.toType(schema)), {
  decode: SchemaGetter.passthrough({ strict: !1 }),
  encode: SchemaGetter.transformOptional(Option.filter((value) => value !== void 0))
})), withStatics = (schema, methods) => Object.defineProperties(schema, Object.getOwnPropertyDescriptors(methods)), statics = (methods) => (schema) => withStatics(schema, methods(schema)), DateTimeUtcFromMillis = Schema.Finite.pipe(Schema.decodeTo(Schema.DateTimeUtc, {
  decode: SchemaGetter.transform((value) => DateTime.makeUnsafe(value)),
  encode: SchemaGetter.transform((value) => DateTime.toEpochMillis(value))
}));

// integrations/opencode/node_modules/@opencode/schema/dist/identifier.js
var length = 26, chars = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz", lastTimestamp = 0, counter = 0;
function ascending() {
  return create(!1);
}
function descending() {
  return create(!0);
}
function create(descending2, timestamp = Date.now()) {
  if (timestamp !== lastTimestamp)
    lastTimestamp = timestamp, counter = 0;
  counter++;
  let current = BigInt(timestamp) * 0x1000n + BigInt(counter), value = descending2 ? ~current : current, time = Array.from({ length: 6 }, (_, index) => Number(value >> BigInt(40 - 8 * index) & 0xffn).toString(16).padStart(2, "0")).join(""), bytes = crypto.getRandomValues(new Uint8Array(length - 12));
  return time + Array.from(bytes, (byte) => chars[byte % 62]).join("");
}

// integrations/opencode/node_modules/@opencode/schema/dist/location.js
var exports_location = {};
__export(exports_location, {
  response: () => response,
  Ref: () => Ref,
  PublicRef: () => PublicRef,
  PublicInfo: () => PublicInfo,
  Location: () => exports_location,
  Info: () => Info
});
import { Schema as Schema4, Struct } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/project-id.js
import { Schema as Schema2 } from "effect";
var ProjectID = Schema2.String.pipe(Schema2.brand("Project.ID"), Schema2.annotate({ identifier: "Project.ID" }), statics((schema) => ({ global: schema.make("global") })));

// integrations/opencode/node_modules/@opencode/schema/dist/workspace-id.js
import { Schema as Schema3 } from "effect";
var WorkspaceID = Schema3.String.check(Schema3.isStartingWith("wrk")).pipe(Schema3.brand("Workspace.ID"), Schema3.annotate({ identifier: "Workspace.ID" }), statics((schema) => {
  let create2 = () => schema.make("wrk_" + ascending());
  return {
    ascending: (id) => {
      if (!id)
        return create2();
      return schema.make(id);
    },
    create: create2
  };
}));

// integrations/opencode/node_modules/@opencode/schema/dist/location.js
var Ref = Schema4.Struct({
  directory: AbsolutePath,
  workspaceID: optional(WorkspaceID)
}).annotate({ identifier: "Location.Ref" }), PublicRef = Schema4.Struct(Struct.omit(Ref.fields, ["workspaceID"])).annotate({
  identifier: "Location.PublicRef"
});

class Info extends Schema4.Class("Location.Info")({
  directory: AbsolutePath,
  workspaceID: optional(WorkspaceID),
  project: Schema4.Struct({
    id: ProjectID,
    directory: AbsolutePath,
    canonical: AbsolutePath
  })
}) {
}
var PublicInfo = Schema4.Struct(Struct.omit(Info.fields, ["workspaceID"])).annotate({
  identifier: "Location.PublicInfo"
});
function response(data) {
  return Schema4.Struct({ location: PublicRef, data });
}

// integrations/opencode/node_modules/@opencode/schema/dist/event.js
var ID = Schema5.String.check(Schema5.isStartingWith("evt_")).pipe(Schema5.brand("Event.ID"), Schema5.annotate({ identifier: "Event.ID" }), statics((schema) => ({ create: () => schema.make("evt_" + ascending()) }))), Seq = Schema5.Int.check(Schema5.isGreaterThanOrEqualTo(0)).pipe(Schema5.brand("Event.Seq"), Schema5.annotate({ identifier: "Event.Seq" })), Version = Schema5.Int.check(Schema5.isGreaterThanOrEqualTo(1)).pipe(Schema5.brand("Event.Version"), Schema5.annotate({ identifier: "Event.Version" })), DurableEnvelope = Schema5.Struct({ aggregateID: Schema5.String, seq: Seq, version: Version });
function ephemeral(input) {
  let data = Schema5.Struct(input.schema);
  return Schema5.Struct({
    id: ID,
    created: Schema5.Finite,
    metadata: optional(Schema5.Record(Schema5.String, Schema5.Unknown)),
    type: Schema5.Literal(input.type),
    location: optional(exports_location.Ref),
    data
  }).annotate({ identifier: input.identifier ?? input.type }).pipe(statics(() => ({
    type: input.type,
    durability: "ephemeral",
    durable: void 0,
    data
  })));
}
function inventory(...definitions) {
  return Object.freeze(definitions);
}

// integrations/opencode/node_modules/@opencode/schema/dist/model.js
var exports_model = {};
__export(exports_model, {
  VariantID: () => VariantID,
  Variant: () => Variant,
  Type: () => Type,
  Settings: () => Settings2,
  Ref: () => Ref3,
  ReasoningField: () => ReasoningField,
  Overlays: () => Overlays2,
  Model: () => exports_model,
  MediaInput: () => MediaInput,
  MaxTokensField: () => MaxTokensField,
  Info: () => Info6,
  ID: () => ID6,
  Family: () => Family,
  Event: () => Event5,
  Cost: () => Cost,
  Compatibility: () => Compatibility,
  Capabilities: () => Capabilities
});
import { Schema as Schema14, Struct as Struct3 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/provider.js
var exports_provider = {};
__export(exports_provider, {
  Transport: () => Transport,
  Settings: () => Settings,
  Request: () => Request,
  Provider: () => exports_provider,
  Package: () => Package,
  Overlays: () => Overlays,
  Info: () => Info5,
  ID: () => ID5,
  Event: () => Event4,
  Encoding: () => Encoding,
  Compaction: () => Compaction,
  CacheWarming: () => CacheWarming,
  CacheTTL: () => CacheTTL,
  Cache: () => Cache,
  Activation: () => Activation
});
import { Effect, Schema as Schema12 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/integration.js
var exports_integration = {};
__export(exports_integration, {
  Ref: () => Ref2,
  OAuthMethod: () => OAuthMethod,
  MethodID: () => MethodID,
  Method: () => Method,
  KeyMethod: () => KeyMethod,
  Integration: () => exports_integration,
  Info: () => Info4,
  ID: () => ID4,
  ExternalMethod: () => ExternalMethod,
  Event: () => Event3,
  EnvMethod: () => EnvMethod,
  CommandMethod: () => CommandMethod,
  CommandAttemptStatus: () => CommandAttemptStatus,
  CommandAttempt: () => CommandAttempt,
  AttemptStatus: () => AttemptStatus,
  AttemptID: () => AttemptID,
  Attempt: () => Attempt
});
import { Schema as Schema10 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/connection.js
var exports_connection = {};
__export(exports_connection, {
  Status: () => Status,
  Info: () => Info2,
  EnvInfo: () => EnvInfo,
  CredentialInfo: () => CredentialInfo,
  Connection: () => exports_connection
});
import { Schema as Schema8 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/credential.js
var exports_credential = {};
__export(exports_credential, {
  Value: () => Value,
  OAuth: () => OAuth,
  Key: () => Key,
  ID: () => ID2,
  External: () => External,
  Event: () => Event,
  Entry: () => Entry,
  Credential: () => exports_credential,
  CreateInput: () => CreateInput
});
import { Schema as Schema7 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/integration-id.js
import { Schema as Schema6 } from "effect";
var IntegrationID = Schema6.String.pipe(Schema6.brand("Integration.ID"), Schema6.annotate({ identifier: "Integration.ID" })), IntegrationMethodID = Schema6.String.pipe(Schema6.brand("Integration.MethodID"), Schema6.annotate({ identifier: "Integration.MethodID" }));

// integrations/opencode/node_modules/@opencode/schema/dist/credential.js
var ID2 = Schema7.String.pipe(Schema7.brand("Credential.ID"), Schema7.annotate({ identifier: "Credential.ID" }), statics((schema) => ({ create: () => schema.make("cred_" + ascending()) }))), Updated = ephemeral({
  type: "credential.updated",
  schema: { integrationID: IntegrationID, credentialID: ID2 }
}), Switched = ephemeral({
  type: "credential.switched",
  schema: { integrationID: IntegrationID, credentialID: Schema7.NullOr(ID2) }
}), Event = {
  Updated,
  Switched,
  Definitions: inventory(Updated, Switched)
}, OAuth = Schema7.Struct({
  type: Schema7.Literal("oauth"),
  methodID: IntegrationMethodID,
  refresh: Schema7.String,
  access: Schema7.String,
  expires: NonNegativeInt,
  metadata: optional(Schema7.Record(Schema7.String, Schema7.Unknown))
}).annotate({ identifier: "Credential.OAuth" }), Key = Schema7.Struct({
  type: Schema7.Literal("key"),
  key: Schema7.String,
  metadata: optional(Schema7.Record(Schema7.String, Schema7.Unknown))
}).annotate({ identifier: "Credential.Key" }), External = Schema7.Struct({
  type: Schema7.Literal("external"),
  methodID: IntegrationMethodID,
  metadata: optional(Schema7.Record(Schema7.String, Schema7.Unknown))
}).annotate({ identifier: "Credential.External" }), Value = Schema7.Union([OAuth, Key, External]).pipe(Schema7.toTaggedUnion("type")).annotate({ identifier: "Credential.Value" }), Entry = Schema7.Struct({
  id: ID2,
  integrationID: IntegrationID,
  label: Schema7.String,
  active: Schema7.Boolean,
  value: Value
}).annotate({ identifier: "Credential.Entry" }), CreateInput = Schema7.Struct({
  id: optional(ID2),
  integrationID: IntegrationID,
  label: optional(Schema7.String),
  value: Value,
  activate: optional(Schema7.Boolean)
}).annotate({ identifier: "Credential.CreateInput" });

// integrations/opencode/node_modules/@opencode/schema/dist/connection.js
var Status = Schema8.Struct({
  status: Schema8.Literals(["needs_auth"]),
  message: Schema8.String,
  url: optional(Schema8.String)
}).annotate({ identifier: "Connection.Status" }), CredentialInfo = Schema8.Struct({
  type: Schema8.Literal("credential"),
  id: exports_credential.ID,
  label: Schema8.String,
  method: Schema8.Literals(["key", "oauth", "external"]),
  status: optional(Status)
}).annotate({ identifier: "Connection.CredentialInfo" }), EnvInfo = Schema8.Struct({
  type: Schema8.Literal("env"),
  name: Schema8.String,
  status: optional(Status)
}).annotate({ identifier: "Connection.EnvInfo" }), Info2 = Schema8.Union([CredentialInfo, EnvInfo]).pipe(Schema8.toTaggedUnion("type")).annotate({ identifier: "Connection.Info" });

// integrations/opencode/node_modules/@opencode/schema/dist/form.js
var exports_form = {};
__export(exports_form, {
  When: () => When,
  Value: () => Value2,
  StringField: () => StringField,
  State: () => State,
  Reply: () => Reply,
  Option: () => Option2,
  NumberField: () => NumberField,
  MultiselectField: () => MultiselectField,
  Metadata: () => Metadata,
  IntegerField: () => IntegerField,
  Info: () => Info3,
  ID: () => ID3,
  Form: () => exports_form,
  Fields: () => Fields,
  Field: () => Field,
  ExternalField: () => ExternalField,
  Event: () => Event2,
  Detail: () => Detail,
  BooleanField: () => BooleanField,
  Answer: () => Answer
});
import { Schema as Schema9 } from "effect";
var IDSchema = Schema9.String.check(Schema9.isStartingWith("frm_")).pipe(Schema9.brand("Form.ID"), Schema9.annotate({ identifier: "Form.ID" })), ID3 = IDSchema.pipe(statics((schema) => ({ create: (id) => schema.make(id ?? "frm_" + ascending()) }))), Metadata = Schema9.Record(Schema9.String, Schema9.Unknown).annotate({ identifier: "Form.Metadata" }), Option2 = Schema9.Struct({
  value: Schema9.String,
  label: Schema9.String,
  description: Schema9.String.pipe(optional)
}).annotate({ identifier: "Form.Option" }), When = Schema9.Struct({
  key: Schema9.String,
  op: Schema9.Literals(["eq", "neq"]),
  value: Schema9.Union([Schema9.String, Schema9.Number, Schema9.Boolean])
}).annotate({ identifier: "Form.When" }), FieldBase = {
  key: Schema9.String,
  title: Schema9.String.pipe(optional),
  description: Schema9.String.pipe(optional),
  required: Schema9.Boolean.pipe(optional),
  hidden: Schema9.Boolean.pipe(optional).annotate({
    description: "Skip the interactive authentication prompt and use the default unless an answer is supplied"
  }),
  when: Schema9.Array(When).pipe(optional)
}, StringField = Schema9.Struct({
  ...FieldBase,
  type: Schema9.Literal("string"),
  format: Schema9.Literals(["email", "uri", "date", "date-time"]).pipe(optional),
  minLength: NonNegativeInt.pipe(optional),
  maxLength: NonNegativeInt.pipe(optional),
  pattern: Schema9.String.pipe(optional),
  placeholder: Schema9.String.pipe(optional),
  default: Schema9.String.pipe(optional),
  options: Schema9.Array(Option2).pipe(optional),
  custom: Schema9.Boolean.pipe(optional)
}).annotate({ identifier: "Form.StringField" }), NumberField = Schema9.Struct({
  ...FieldBase,
  type: Schema9.Literal("number"),
  minimum: Schema9.Number.pipe(optional),
  maximum: Schema9.Number.pipe(optional),
  default: Schema9.Number.pipe(optional)
}).annotate({ identifier: "Form.NumberField" }), IntegerField = Schema9.Struct({
  ...FieldBase,
  type: Schema9.Literal("integer"),
  minimum: Schema9.Number.pipe(optional),
  maximum: Schema9.Number.pipe(optional),
  default: Schema9.Number.pipe(optional)
}).annotate({ identifier: "Form.IntegerField" }), BooleanField = Schema9.Struct({
  ...FieldBase,
  type: Schema9.Literal("boolean"),
  default: Schema9.Boolean.pipe(optional)
}).annotate({ identifier: "Form.BooleanField" }), MultiselectField = Schema9.Struct({
  ...FieldBase,
  type: Schema9.Literal("multiselect"),
  options: Schema9.Array(Option2),
  minItems: NonNegativeInt.pipe(optional),
  maxItems: NonNegativeInt.pipe(optional),
  custom: Schema9.Boolean.pipe(optional),
  default: Schema9.Array(Schema9.String).pipe(optional)
}).annotate({ identifier: "Form.MultiselectField" }), ExternalField = Schema9.Struct({
  key: Schema9.String,
  type: Schema9.Literal("external"),
  url: Schema9.String,
  title: Schema9.String.pipe(optional),
  description: Schema9.String.pipe(optional)
}).annotate({ identifier: "Form.ExternalField" }), Field = Schema9.Union([
  StringField,
  NumberField,
  IntegerField,
  BooleanField,
  MultiselectField,
  ExternalField
]).pipe(Schema9.toTaggedUnion("type"), Schema9.annotate({ identifier: "Form.Field" })), Fields = Schema9.NonEmptyArray(Field).annotate({ identifier: "Form.Fields" }), InfoBase = {
  id: ID3,
  sessionID: Schema9.String,
  title: Schema9.String,
  metadata: Metadata.pipe(optional)
}, Info3 = Schema9.Struct({
  ...InfoBase,
  fields: Fields
}).annotate({ identifier: "Form.Info" }), Value2 = Schema9.Union([Schema9.String, Schema9.Number, Schema9.Boolean, Schema9.Array(Schema9.String)]).annotate({
  identifier: "Form.Value"
}), Answer = Schema9.Record(Schema9.String, Value2).annotate({ identifier: "Form.Answer" }), State = Schema9.Union([
  Schema9.Struct({ status: Schema9.Literal("pending") }),
  Schema9.Struct({ status: Schema9.Literal("answered"), answer: Answer }),
  Schema9.Struct({ status: Schema9.Literal("cancelled"), message: Schema9.String.pipe(optional) })
]).pipe(Schema9.toTaggedUnion("status")).annotate({ identifier: "Form.State" }), Detail = Schema9.Struct({
  ...Info3.fields,
  state: State
}).annotate({ identifier: "Form.Detail" }), Reply = Schema9.Struct({
  answer: Answer
}).annotate({ identifier: "Form.Reply" }), Created = ephemeral({ type: "form.created", schema: { form: Info3 } }), Replied = ephemeral({ type: "form.replied", schema: { id: ID3, sessionID: Schema9.String, answer: Answer } }), Cancelled = ephemeral({ type: "form.cancelled", schema: { id: ID3, sessionID: Schema9.String } }), Event2 = { Created, Replied, Cancelled, Definitions: inventory(Created, Replied, Cancelled) };

// integrations/opencode/node_modules/@opencode/schema/dist/integration.js
var ID4 = IntegrationID, MethodID = IntegrationMethodID, OAuthMethod = Schema10.Struct({
  id: MethodID,
  type: Schema10.Literal("oauth"),
  label: Schema10.String,
  form: optional(exports_form.Fields)
}).annotate({ identifier: "Integration.OAuthMethod" }), CommandMethod = Schema10.Struct({
  id: MethodID,
  type: Schema10.Literal("command"),
  label: Schema10.String,
  command: Schema10.Array(Schema10.String)
}).annotate({ identifier: "Integration.CommandMethod" }), KeyMethod = Schema10.Struct({
  type: Schema10.Literal("key"),
  label: optional(Schema10.String),
  form: optional(exports_form.Fields)
}).annotate({ identifier: "Integration.KeyMethod" }), ExternalMethod = Schema10.Struct({
  id: MethodID,
  type: Schema10.Literal("external"),
  label: Schema10.String,
  form: optional(exports_form.Fields)
}).annotate({ identifier: "Integration.ExternalMethod" }), EnvMethod = Schema10.Struct({
  type: Schema10.Literal("env"),
  names: Schema10.Array(Schema10.String)
}).annotate({ identifier: "Integration.EnvMethod" }), Method = Schema10.Union([OAuthMethod, CommandMethod, KeyMethod, ExternalMethod, EnvMethod]).pipe(Schema10.toTaggedUnion("type")).annotate({ identifier: "Integration.Method" }), Updated2 = ephemeral({
  type: "integration.updated",
  schema: {}
}), Event3 = { Updated: Updated2, Definitions: inventory(Updated2) }, Ref2 = Schema10.Struct({
  id: ID4,
  name: Schema10.String,
  metadata: optional(Schema10.Record(Schema10.String, Schema10.Any))
}).annotate({ identifier: "Integration.Ref" }), Info4 = Schema10.Struct({
  id: ID4,
  name: Schema10.String,
  metadata: optional(Schema10.Record(Schema10.String, Schema10.Any)),
  methods: Schema10.Array(Method),
  connections: Schema10.Array(exports_connection.Info)
}).annotate({ identifier: "Integration.Info" }), AttemptID = Schema10.String.pipe(Schema10.brand("Integration.AttemptID"), Schema10.annotate({ identifier: "Integration.AttemptID" }), statics((schema) => ({ create: () => schema.make("con_" + ascending()) }))), AttemptTime = Schema10.Struct({
  created: Schema10.Number,
  expires: Schema10.Number
});

class Attempt extends Schema10.Class("Integration.Attempt")({
  attemptID: AttemptID,
  url: Schema10.String,
  instructions: Schema10.String,
  mode: Schema10.Literals(["auto", "code"]),
  paste: Schema10.optionalKey(Schema10.Literal(!0)),
  time: AttemptTime
}) {
}
var AttemptStatus = Schema10.Union([
  Schema10.Struct({ status: Schema10.Literal("pending"), time: AttemptTime }),
  Schema10.Struct({ status: Schema10.Literal("complete"), time: AttemptTime }),
  Schema10.Struct({ status: Schema10.Literal("failed"), message: Schema10.String, time: AttemptTime }),
  Schema10.Struct({ status: Schema10.Literal("expired"), time: AttemptTime })
]).pipe(Schema10.toTaggedUnion("status")).annotate({ identifier: "Integration.AttemptStatus" }), CommandAttempt = Schema10.Struct({
  attemptID: AttemptID,
  time: AttemptTime
}).annotate({ identifier: "Integration.CommandAttempt" }), CommandAttemptStatus = Schema10.Union([
  Schema10.Struct({ status: Schema10.Literal("pending"), message: optional(Schema10.String), time: AttemptTime }),
  Schema10.Struct({ status: Schema10.Literal("complete"), time: AttemptTime }),
  Schema10.Struct({ status: Schema10.Literal("failed"), message: Schema10.String, time: AttemptTime }),
  Schema10.Struct({ status: Schema10.Literal("expired"), time: AttemptTime })
]).pipe(Schema10.toTaggedUnion("status")).annotate({ identifier: "Integration.CommandAttemptStatus" });

// integrations/opencode/node_modules/@opencode/schema/dist/model-compatibility.js
import { Schema as Schema11, Struct as Struct2 } from "effect";
var ReasoningField = Schema11.Union([
  Schema11.Literals(["reasoning", "reasoning_content", "reasoning_text"]),
  Schema11.String
]).annotate({ identifier: "Model.ReasoningField" }), MaxTokensField = Schema11.Literals(["max_completion_tokens", "max_tokens"]).annotate({
  identifier: "Model.MaxTokensField"
}), MediaInput = Schema11.Literals(["assets", "state", "root"]).annotate({
  identifier: "Model.MediaInput"
}), CacheFormat = Schema11.Literals(["anthropic", "alibaba"]).annotate({ identifier: "Model.CacheFormat" }), Compatibility = Schema11.Struct({
  reasoningField: ReasoningField.pipe(optional),
  requireReasoning: Schema11.Boolean.pipe(optional),
  maxTokensField: MaxTokensField.pipe(optional),
  requireFinishReason: Schema11.Boolean.pipe(optional),
  requireAssistantAfterTool: Schema11.Boolean.pipe(optional),
  supportsPromptCacheKey: Schema11.Boolean.pipe(optional),
  streamUsage: Schema11.Boolean.pipe(optional),
  cacheFormat: CacheFormat.pipe(optional),
  supportsThinkingBlockBinding: Schema11.Boolean.pipe(optional),
  supportsEffortUpdates: Schema11.Boolean.pipe(optional),
  mediaInput: MediaInput.pipe(optional)
}).annotate({ identifier: "Model.Compatibility" }), ProviderCompatibility = Schema11.Struct(Struct2.omit(Compatibility.fields, ["supportsThinkingBlockBinding", "supportsEffortUpdates"])).annotate({ identifier: "Provider.Compatibility" });

// integrations/opencode/node_modules/@opencode/schema/dist/provider.js
var ID5 = Schema12.String.pipe(Schema12.brand("Provider.ID"), Schema12.annotate({ identifier: "Provider.ID" }), statics((schema) => ({
  opencode: schema.make("opencode"),
  anthropic: schema.make("anthropic"),
  openai: schema.make("openai"),
  google: schema.make("google"),
  googleVertex: schema.make("google-vertex"),
  githubCopilot: schema.make("github-copilot"),
  claudeAgent: schema.make("claude-agent"),
  amazonBedrock: schema.make("amazon-bedrock"),
  azure: schema.make("azure"),
  openrouter: schema.make("openrouter"),
  mistral: schema.make("mistral"),
  gitlab: schema.make("gitlab")
}))), Updated3 = ephemeral({ type: "provider.updated", schema: {} }), Event4 = { Updated: Updated3, Definitions: inventory(Updated3) }, Package = Schema12.String, Activation = Schema12.Literals(["auto", "enabled", "disabled"]), Compaction = Schema12.Union([
  Schema12.Struct({ type: Schema12.Literal("summary") }),
  Schema12.Struct({ type: Schema12.Literal("native") })
]).pipe(Schema12.toTaggedUnion("type")).annotate({ identifier: "Provider.Compaction" }), Transport = Schema12.Literals(["http", "websocket"]).annotate({ identifier: "Provider.Transport" }), CacheTTL = Schema12.TemplateLiteral([Schema12.Number, Schema12.Literals(["ms", "s", "m", "h", "d"])]).check(Schema12.isPattern(/^\d+(ms|s|m|h|d)$/)).annotate({ identifier: "Provider.CacheTTL" }), CacheWarming = Schema12.Union([
  Schema12.Boolean,
  Schema12.Struct({
    interval: Schema12.Union([Schema12.Literal("auto"), Schema12.String]).pipe(optional).annotate({ description: `Duration such as "4 minutes", or "auto" (default) for 90% of the model's cache.ttl` }),
    duration: Schema12.String.pipe(optional).annotate({ description: 'Duration such as "30 minutes"' })
  })
]).annotate({ identifier: "Provider.CacheWarming" }), Cache = Schema12.Struct({
  ttl: CacheTTL.pipe(optional),
  warming: CacheWarming.pipe(optional)
}).annotate({ identifier: "Provider.Cache" }), Encoding = Schema12.Literals(["zstd", "gzip", "none"]).annotate({ identifier: "Provider.Encoding" }), Timeout = Schema12.Union([Schema12.Finite, Schema12.Literal(!1)]).pipe(optional), Settings = Schema12.StructWithRest(Schema12.Struct({
  timeout: Timeout,
  headerTimeout: Timeout,
  chunkTimeout: Timeout,
  compaction: Compaction.pipe(optional),
  cache: Cache.pipe(optional),
  transport: Transport.pipe(optional),
  encoding: Encoding.pipe(optional)
}), [Schema12.Record(Schema12.String, Schema12.Any)]).annotate({ identifier: "Provider.Settings" }), Overlays = {
  settings: Settings.pipe(optional),
  headers: Schema12.Record(Schema12.String, Schema12.String).pipe(optional),
  body: Schema12.Record(Schema12.String, Schema12.Any).pipe(optional)
}, Request = Schema12.Struct({
  settings: Settings.pipe(Schema12.withConstructorDefault(Effect.succeed({}))),
  headers: Schema12.Record(Schema12.String, Schema12.String),
  body: Schema12.Record(Schema12.String, Schema12.Any)
}).annotate({ identifier: "Provider.Request" }), Info5 = Schema12.Struct({
  id: ID5,
  canonical: ID5.pipe(optional),
  integrationID: exports_integration.ID.pipe(optional),
  name: Schema12.String,
  activation: Activation,
  package: Package,
  compatibility: ProviderCompatibility.pipe(optional),
  ...Overlays
}).annotate({ identifier: "Provider.Info" }).pipe(statics(() => ({
  empty: (id) => ({ id, name: id, activation: "auto", package: "" })
})));

// integrations/opencode/node_modules/@opencode/schema/dist/money.js
var exports_money = {};
__export(exports_money, {
  USDPerMillionTokens: () => USDPerMillionTokens,
  USD: () => USD,
  Money: () => exports_money
});
import { Schema as Schema13 } from "effect";
var USD = Schema13.Finite.pipe(Schema13.brand("Money.USD"), Schema13.annotate({ identifier: "Money.USD" }), statics((schema) => ({ zero: schema.make(0) }))), USDPerMillionTokens = Schema13.Finite.pipe(Schema13.brand("Money.USDPerMillionTokens"), Schema13.annotate({ identifier: "Money.USDPerMillionTokens" }), statics((schema) => ({ zero: schema.make(0) })));

// integrations/opencode/node_modules/@opencode/schema/dist/model.js
var ID6 = Schema14.String.pipe(Schema14.brand("Model.ID")), Updated4 = ephemeral({ type: "model.updated", schema: {} }), Event5 = { Updated: Updated4, Definitions: inventory(Updated4) }, VariantID = Schema14.String.pipe(Schema14.brand("Model.VariantID")), Ref3 = Schema14.Struct({
  id: ID6,
  providerID: exports_provider.ID,
  variant: VariantID.pipe(optional)
}).annotate({ identifier: "Model.Ref" }).pipe(statics((schema) => ({
  parse: (input) => {
    let providerEnd = input.indexOf("/");
    if (providerEnd <= 0)
      throw Error(`Invalid model reference: ${input}`);
    let providerID = input.slice(0, providerEnd), variantStart = input.indexOf("#", providerEnd + 1), id = input.slice(providerEnd + 1, variantStart === -1 ? void 0 : variantStart), variant = variantStart === -1 ? void 0 : input.slice(variantStart + 1);
    if (!id || providerID.includes("#") || variant !== void 0 && (!variant || variant.includes("#")))
      throw Error(`Invalid model reference: ${input}`);
    return schema.make({
      providerID: exports_provider.ID.make(providerID),
      id: ID6.make(id),
      ...variant ? { variant: VariantID.make(variant) } : {}
    });
  }
})));
var Family = Schema14.String.pipe(Schema14.brand("Model.Family")), Type = Schema14.Union([
  Schema14.Literals(["chat", "decision", "image"]),
  Schema14.String
]).annotate({ identifier: "Model.Type" }), Settings2 = Schema14.StructWithRest(Schema14.Struct(Struct3.pick(exports_provider.Settings.schema.fields, ["compaction", "cache", "encoding"])), [Schema14.Record(Schema14.String, Schema14.Any)]).annotate({ identifier: "Model.Settings" }), VariantSettings = Schema14.StructWithRest(Schema14.Struct(Struct3.omit(Settings2.schema.fields, ["cache"])), Settings2.records).annotate({ identifier: "Model.VariantSettings" }), Overlays2 = {
  settings: Settings2.pipe(optional),
  headers: Schema14.Record(Schema14.String, Schema14.String).pipe(optional),
  body: Schema14.Record(Schema14.String, Schema14.Any).pipe(optional)
}, Capabilities = Schema14.Struct({
  tools: Schema14.Boolean,
  input: Schema14.Array(Schema14.String),
  output: Schema14.Array(Schema14.String),
  temperature: Schema14.Boolean.pipe(optional)
}).annotate({ identifier: "Model.Capabilities" }).pipe(statics(() => ({
  default: () => ({ tools: !0, input: ["text", "image"], output: ["text"] })
}))), Cost = Schema14.Struct({
  tier: Schema14.Struct({
    type: Schema14.tag("context"),
    size: Schema14.Int
  }).pipe(optional),
  input: exports_money.USDPerMillionTokens,
  output: exports_money.USDPerMillionTokens,
  cache: Schema14.Struct({
    read: exports_money.USDPerMillionTokens,
    write: exports_money.USDPerMillionTokens
  })
}).annotate({ identifier: "Model.Cost" }), Variant = Schema14.Struct({
  id: VariantID,
  ...Overlays2,
  settings: VariantSettings.pipe(optional)
}).annotate({ identifier: "Model.Variant" }), Info6 = Schema14.Struct({
  id: ID6,
  modelID: ID6,
  providerID: exports_provider.ID,
  canonical: exports_provider.ID.pipe(optional),
  type: Type.pipe(optional),
  family: Family.pipe(optional),
  name: Schema14.String,
  compatibility: Compatibility.pipe(optional),
  package: exports_provider.Package.pipe(optional),
  ...Overlays2,
  capabilities: Capabilities,
  variants: Schema14.Array(Variant),
  time: Schema14.Struct({
    released: Schema14.Finite
  }),
  cost: Schema14.Array(Cost),
  status: Schema14.Literals(["alpha", "beta", "deprecated", "active"]),
  enabled: Schema14.Boolean,
  limit: Schema14.Struct({
    context: Schema14.Int,
    input: Schema14.Int.pipe(optional),
    output: Schema14.Int
  })
}).annotate({ identifier: "Model.Info" }).pipe(statics(() => ({
  default: (providerID, id) => ({
    id,
    modelID: id,
    providerID,
    name: id,
    capabilities: Capabilities.default(),
    variants: [],
    time: { released: 0 },
    cost: [],
    status: "active",
    enabled: !0,
    limit: { context: 200000, output: 32000 }
  })
})));

// integrations/opencode/node_modules/@opencode/schema/dist/permission.js
var exports_permission = {};
__export(exports_permission, {
  Source: () => Source,
  Ruleset: () => Ruleset,
  Rule: () => Rule,
  Request: () => Request2,
  Reply: () => Reply2,
  Permission: () => exports_permission,
  ID: () => ID7,
  Event: () => Event6,
  Effect: () => Effect2
});
import { Schema as Schema16 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/session-id.js
import { Schema as Schema15 } from "effect";
var SessionID = Schema15.String.check(Schema15.isStartingWith("ses")).pipe(Schema15.brand("SessionID"), Schema15.annotate({ identifier: "Session.ID" }), statics((schema) => ({
  create: () => schema.make("ses_" + descending())
})));

// integrations/opencode/node_modules/@opencode/schema/dist/permission.js
var ID7 = Schema16.String.check(Schema16.isStartingWith("per")).pipe(Schema16.brand("Permission.ID"), Schema16.annotate({ identifier: "Permission.ID" }), statics((schema) => ({ create: (id) => schema.make(id ?? "per_" + ascending()) }))), Source = Schema16.Union([
  Schema16.Struct({
    type: Schema16.Literal("tool"),
    messageID: Schema16.String,
    id: Schema16.String
  })
]).annotate({ identifier: "Permission.Source" }), RequestFields = {
  sessionID: SessionID,
  action: Schema16.String,
  resources: Schema16.Array(Schema16.String),
  save: Schema16.Array(Schema16.String).pipe(optional),
  metadata: Schema16.Record(Schema16.String, Schema16.Unknown).pipe(optional),
  source: Source.pipe(optional),
  message: Schema16.String.pipe(optional)
}, Request2 = Schema16.Struct({
  id: ID7,
  ...RequestFields
}).annotate({ identifier: "Permission.Request" }), Reply2 = Schema16.Literals(["once", "always", "reject"]).annotate({ identifier: "Permission.Reply" }), Asked = ephemeral({ type: "permission.asked", schema: Request2.fields }), Replied2 = ephemeral({
  type: "permission.replied",
  schema: {
    sessionID: SessionID,
    requestID: ID7,
    reply: Reply2
  }
}), Event6 = { Asked, Replied: Replied2, Definitions: inventory(Asked, Replied2) }, Effect2 = Schema16.Literals(["allow", "deny", "ask"]).annotate({ identifier: "Permission.Effect" }), Rule = Schema16.Struct({
  action: Schema16.String,
  resource: Schema16.String,
  effect: Effect2
}).annotate({ identifier: "Permission.Rule" }), Ruleset = Schema16.Array(Rule).annotate({ identifier: "Permission.Ruleset" });

// integrations/opencode/node_modules/@opencode/schema/dist/agent.js
var Updated5 = ephemeral({ type: "agent.updated", schema: {} }), ID8 = Schema17.String.pipe(Schema17.brand("Agent.ID"), Schema17.annotate({ identifier: "Agent.ID" })), Name = Schema17.String.pipe(Schema17.brand("Agent.Name")), Color = Schema17.String.annotate({ identifier: "Agent.Color" }), Info7 = Schema17.Struct({
  id: ID8,
  name: Name,
  model: exports_model.Ref.pipe(optional),
  request: exports_provider.Request,
  system: Schema17.String.pipe(optional),
  description: Schema17.String.pipe(optional),
  mode: Schema17.Literals(["subagent", "primary", "all"]),
  hidden: Schema17.Boolean,
  color: Color.pipe(optional),
  steps: PositiveInt.pipe(optional),
  permissions: exports_permission.Ruleset
}).annotate({ identifier: "Agent.Info" }).pipe(statics(() => ({
  default: (id) => ({
    id,
    name: Name.make(id),
    request: { settings: {}, headers: {}, body: {} },
    mode: "primary",
    hidden: !1,
    permissions: [
      { action: "*", resource: "*", effect: "allow" },
      { action: "read", resource: "*.env", effect: "ask" },
      { action: "read", resource: "*.env.*", effect: "ask" },
      { action: "read", resource: "*.env.example", effect: "allow" }
    ]
  })
}))), Event7 = {
  Updated: Updated5,
  Definitions: inventory(Updated5)
};

// integrations/opencode/node_modules/@opencode/schema/dist/session-message.js
var exports_session_message = {};
__export(exports_session_message, {
  User: () => User,
  ToolStateStreaming: () => ToolStateStreaming,
  ToolStateRunning: () => ToolStateRunning,
  ToolStateError: () => ToolStateError,
  ToolStateCompleted: () => ToolStateCompleted,
  ToolState: () => ToolState,
  System: () => System,
  Synthetic: () => Synthetic,
  Skill: () => Skill,
  Shell: () => Shell,
  SessionMessage: () => exports_session_message,
  Native: () => Native,
  ModelSelected: () => ModelSelected,
  MediaUser: () => MediaUser,
  MediaInfo: () => MediaInfo,
  LocationSwitched: () => LocationSwitched,
  Info: () => Info12,
  Idle: () => Idle,
  ID: () => ID13,
  CompactionRunning: () => CompactionRunning,
  CompactionFailed: () => CompactionFailed,
  CompactionCompleted: () => CompactionCompleted,
  Compaction: () => Compaction2,
  AssistantTool: () => AssistantTool,
  AssistantText: () => AssistantText,
  AssistantRetry: () => AssistantRetry,
  AssistantReasoning: () => AssistantReasoning,
  AssistantContentEncoded: () => AssistantContentEncoded,
  AssistantContent: () => AssistantContent,
  Assistant: () => Assistant,
  AgentSelected: () => AgentSelected
});
import { Schema as Schema27 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/tool.js
var exports_tool = {};
__export(exports_tool, {
  Tool: () => exports_tool,
  TextContent: () => TextContent,
  FileContent: () => FileContent,
  Error: () => Error2,
  Content: () => Content,
  CallID: () => CallID
});
import { Effect as Effect3, JsonSchema, Schema as Schema18 } from "effect";
var CallID = Schema18.String.pipe(Schema18.brand("Tool.CallID"));

class Error2 extends Schema18.TaggedError()("Tool.Error", {
  message: Schema18.String,
  error: Schema18.optional(Schema18.Defect()),
  metadata: Schema18.optional(Schema18.Record(Schema18.String, Schema18.Unknown))
}) {
}
var TextContent = Schema18.Struct({
  type: Schema18.Literal("text"),
  text: Schema18.String
}).annotate({ identifier: "Tool.TextContent" }), FileContent = Schema18.Struct({
  type: Schema18.Literal("file"),
  uri: Schema18.String,
  mime: Schema18.String,
  name: Schema18.optional(Schema18.String)
}).annotate({ identifier: "Tool.FileContent" }), Content = Schema18.Union([TextContent, FileContent]).pipe(Schema18.toTaggedUnion("type")).annotate({ identifier: "Tool.Content" });

// integrations/opencode/node_modules/@opencode/schema/dist/project.js
var exports_project = {};
__export(exports_project, {
  Vcs: () => Vcs,
  UpdateInput: () => UpdateInput,
  Time: () => Time,
  Project: () => exports_project,
  Info: () => Info8,
  Icon: () => Icon,
  ID: () => ID9,
  Event: () => Event8,
  Current: () => Current,
  Commands: () => Commands
});
import { Schema as Schema19 } from "effect";
var ID9 = ProjectID, Vcs = Schema19.String.check(Schema19.isPattern(/^[a-z][a-z0-9._-]*$/u)).annotate({
  identifier: "Project.Vcs"
}), Current = Schema19.Struct({
  id: ID9,
  directory: AbsolutePath,
  canonical: AbsolutePath
}).annotate({ identifier: "Project.Current" }), Icon = Schema19.Struct({
  url: optional(Schema19.String),
  override: optional(Schema19.String),
  color: optional(Schema19.String)
}).annotate({ identifier: "Project.Icon" }), Commands = Schema19.Struct({
  start: optional(Schema19.String.annotate({ description: "Startup script to run when creating a new workspace (worktree)" }))
}).annotate({ identifier: "Project.Commands" }), Time = Schema19.Struct({
  created: NonNegativeInt,
  updated: NonNegativeInt,
  active: NonNegativeInt,
  archived: optional(NonNegativeInt)
}).annotate({ identifier: "Project.Time" }), Info8 = Schema19.Struct({
  id: ID9,
  canonical: AbsolutePath,
  vcs: optional(Vcs),
  name: optional(Schema19.String),
  icon: optional(Icon),
  commands: optional(Commands),
  time: Time,
  sandboxes: Schema19.Array(Schema19.String)
}).annotate({ identifier: "Project" }), UpdateInput = Schema19.Struct({
  projectID: ID9,
  canonical: optional(AbsolutePath),
  name: optional(Schema19.String),
  icon: optional(Icon),
  commands: optional(Commands)
}).annotate({ identifier: "Project.UpdateInput" }), Updated6 = ephemeral({ type: "project.updated", schema: Info8.fields }), Event8 = { Updated: Updated6, Definitions: inventory(Updated6) };

// integrations/opencode/node_modules/@opencode/schema/dist/prompt.js
import { Schema as Schema21 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/skill.js
var exports_skill = {};
__export(exports_skill, {
  UrlSource: () => UrlSource,
  Source: () => Source2,
  Skill: () => exports_skill,
  Name: () => Name2,
  Info: () => Info9,
  ID: () => ID10,
  Event: () => Event9,
  EmbeddedSource: () => EmbeddedSource,
  DirectorySource: () => DirectorySource
});
import { Schema as Schema20 } from "effect";
var ID10 = Schema20.String.pipe(Schema20.brand("Skill.ID"), Schema20.annotate({ identifier: "Skill.ID" })), Name2 = Schema20.String.pipe(Schema20.brand("Skill.Name"), Schema20.annotate({ identifier: "Skill.Name" })), DirectorySource = Schema20.Struct({
  type: Schema20.tag("directory"),
  path: AbsolutePath
}).annotate({ identifier: "Skill.DirectorySource" }), UrlSource = Schema20.Struct({
  type: Schema20.tag("url"),
  url: Schema20.String
}).annotate({ identifier: "Skill.UrlSource" }), Info9 = Schema20.Struct({
  id: ID10,
  name: Name2,
  description: Schema20.String.pipe(optional),
  autoinvoke: Schema20.Boolean.pipe(optional),
  path: AbsolutePath,
  content: Schema20.String
}).annotate({ identifier: "Skill.Info" }), Updated7 = ephemeral({ type: "skill.updated", schema: {} }), Event9 = { Updated: Updated7, Definitions: inventory(Updated7) }, EmbeddedSource = Schema20.Struct({
  type: Schema20.tag("embedded"),
  skill: Schema20.suspend(() => Info9)
}).annotate({ identifier: "Skill.EmbeddedSource" }), Source2 = Object.assign(Schema20.Union([DirectorySource, UrlSource, EmbeddedSource]).pipe(Schema20.toTaggedUnion("type"), Schema20.annotate({ identifier: "Skill.Source" })), {
  equals: (a, b) => {
    if (a.type !== b.type)
      return !1;
    if (a.type === "directory" && b.type === "directory")
      return a.path === b.path;
    if (a.type === "url" && b.type === "url")
      return a.url === b.url;
    if (a.type === "embedded" && b.type === "embedded")
      return a.skill.id === b.skill.id;
    return !1;
  },
  key: (source) => source.type === "directory" ? `directory:${source.path}` : source.type === "url" ? `url:${source.url}` : `embedded:${source.skill.id}`
});

// integrations/opencode/node_modules/@opencode/schema/dist/prompt.js
var PromptMention = Schema21.Struct({
  start: Schema21.Finite,
  end: Schema21.Finite,
  text: Schema21.String
}).annotate({ identifier: "Prompt.Mention" }), FileSource = Schema21.Union([
  Schema21.Struct({ type: Schema21.Literal("inline") }),
  Schema21.Struct({ type: Schema21.Literal("uri"), uri: Schema21.String })
]).pipe(Schema21.toTaggedUnion("type")).annotate({ identifier: "Prompt.FileSource" }), Base64 = Schema21.String.check(Schema21.makeFilter((value) => value.length % 4 === 0 && /^[A-Za-z0-9+/]*={0,2}$/u.test(value), Schema21.isPattern(/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u).annotations)).annotate({ identifier: "Prompt.Base64" }), FileAttachment = Schema21.Struct({
  data: Base64,
  mime: Schema21.String,
  source: FileSource,
  name: Schema21.String.pipe(optional),
  description: Schema21.String.pipe(optional),
  mention: PromptMention.pipe(optional)
}).annotate({ identifier: "Prompt.FileAttachment" }).pipe(statics((schema) => ({
  create: (input) => schema.make({
    data: input.data,
    mime: input.mime,
    source: input.source,
    name: input.name,
    description: input.description,
    mention: input.mention
  })
}))), MediaRef = Schema21.String.check(Schema21.isPattern(/^media:[0-9a-f]{64}$/)).annotate({
  identifier: "Prompt.MediaRef"
}), { data: _, ...fileAttachmentFields } = FileAttachment.fields, MediaFileAttachment = Schema21.Struct({
  ...fileAttachmentFields,
  uri: MediaRef,
  bytes: Schema21.Int.pipe(optional),
  width: Schema21.Int.pipe(optional),
  height: Schema21.Int.pipe(optional)
}).annotate({ identifier: "Prompt.MediaFileAttachment" }), Attachment = Schema21.Union([FileAttachment, MediaFileAttachment]).annotate({
  identifier: "Prompt.Attachment"
}), AgentAttachment = Schema21.Struct({
  name: Schema21.String,
  mention: PromptMention.pipe(optional)
}).annotate({ identifier: "Prompt.AgentAttachment" }), SkillAttachment = Schema21.Struct({
  id: exports_skill.ID,
  name: exports_skill.Name,
  text: Schema21.String.pipe(optional),
  mention: PromptMention.pipe(optional)
}).annotate({ identifier: "Prompt.SkillAttachment" }), Prompt = Schema21.Struct({
  text: Schema21.String,
  files: Schema21.Array(FileAttachment).pipe(optional),
  agents: Schema21.Array(AgentAttachment).pipe(optional),
  skills: Schema21.Array(SkillAttachment).pipe(optional)
}).annotate({ identifier: "Prompt" }).pipe(statics((schema) => ({
  equivalence: Schema21.toEquivalence(schema),
  fromUserMessage: (input) => schema.make({
    text: input.text,
    ...input.files === void 0 ? {} : { files: input.files },
    ...input.agents === void 0 ? {} : { agents: input.agents },
    ...input.skills === void 0 ? {} : { skills: input.skills }
  })
})));

// integrations/opencode/node_modules/@opencode/schema/dist/shell.js
var exports_shell = {};
__export(exports_shell, {
  Time: () => Time2,
  Status: () => Status2,
  Shell: () => exports_shell,
  OutputInput: () => OutputInput,
  Output: () => Output,
  Metadata: () => Metadata2,
  Info: () => Info10,
  ID: () => ID11,
  Event: () => Event10,
  CreateInput: () => CreateInput2
});
import { Schema as Schema22 } from "effect";
var IDSchema2 = Schema22.String.check(Schema22.isStartingWith("sh_")).pipe(Schema22.brand("Shell.ID"), Schema22.annotate({ identifier: "Shell.ID" })), ID11 = IDSchema2.pipe(statics((schema) => {
  let create2 = () => schema.make("sh_" + ascending());
  return {
    create: create2,
    ascending: (id) => id === void 0 ? create2() : schema.make(id)
  };
})), Status2 = Schema22.Literals(["running", "exited", "timeout", "killed"]), Time2 = Schema22.Struct({
  started: Schema22.Finite.annotate({ description: "Start time in milliseconds since the Unix epoch" }),
  completed: optional(Schema22.Finite).annotate({
    description: "Completion time in milliseconds since the Unix epoch"
  })
}), Metadata2 = Schema22.Record(Schema22.String, Schema22.Unknown), Info10 = Schema22.Struct({
  id: ID11,
  status: Status2,
  command: Schema22.String,
  cwd: Schema22.String,
  shell: Schema22.String,
  file: Schema22.String,
  pid: optional(NonNegativeInt),
  exit: optional(Schema22.Finite),
  signal: optional(Schema22.String),
  metadata: Metadata2,
  time: Time2
}).annotate({ identifier: "Shell.Info" }), Created2 = ephemeral({ type: "shell.created", schema: { info: Info10 } }), Exited = ephemeral({ type: "shell.exited", schema: { id: ID11, exit: optional(Schema22.Finite), status: Status2 } }), Deleted = ephemeral({ type: "shell.deleted", schema: { id: ID11 } }), Event10 = { Created: Created2, Exited, Deleted, Definitions: inventory(Created2, Exited, Deleted) }, CreateInput2 = Schema22.Struct({
  command: Schema22.String,
  cwd: optional(Schema22.String),
  timeout: optional(NonNegativeInt),
  metadata: optional(Metadata2)
}), OutputInput = Schema22.Struct({
  cursor: optional(NonNegativeInt),
  limit: optional(NonNegativeInt)
}), Output = Schema22.Struct({
  output: Schema22.String,
  cursor: NonNegativeInt,
  size: NonNegativeInt,
  truncated: Schema22.Boolean
});

// integrations/opencode/node_modules/@opencode/schema/dist/llm.js
import { Schema as Schema23 } from "effect";
var FinishReason = Schema23.Literals(["stop", "length", "tool-calls", "content-filter", "error", "unknown"]);

// integrations/opencode/node_modules/@opencode/schema/dist/session-error.js
var exports_session_error = {};
__export(exports_session_error, {
  SessionError: () => exports_session_error,
  Error: () => Error3
});
import { Schema as Schema24 } from "effect";
var Error3 = Schema24.Struct({
  type: Schema24.String,
  message: Schema24.String,
  status: Schema24.Int.check(Schema24.isBetween({ minimum: 100, maximum: 599 })).pipe(optional),
  response: Schema24.Struct({ body: Schema24.String }).pipe(optional),
  retryAfterMs: NonNegativeInt.pipe(optional)
}).annotate({ identifier: "Session.StructuredError" });

// integrations/opencode/node_modules/@opencode/schema/dist/snapshot.js
var exports_snapshot = {};
__export(exports_snapshot, {
  Snapshot: () => exports_snapshot,
  ID: () => ID12
});
import { Schema as Schema25 } from "effect";
var ID12 = Schema25.String.pipe(Schema25.brand("Snapshot.ID"), Schema25.annotate({ identifier: "Snapshot.ID" }));

// integrations/opencode/node_modules/@opencode/schema/dist/token-usage.js
var exports_token_usage = {};
__export(exports_token_usage, {
  total: () => total,
  TokenUsage: () => exports_token_usage,
  Info: () => Info11
});
import { Schema as Schema26 } from "effect";
var Info11 = Schema26.Struct({
  input: Schema26.Finite,
  output: Schema26.Finite,
  reasoning: Schema26.Finite,
  cache: Schema26.Struct({
    read: Schema26.Finite,
    write: Schema26.Finite
  })
}).annotate({ identifier: "TokenUsage.Info" });
function total(tokens) {
  return tokens.input + tokens.output + tokens.reasoning + tokens.cache.read + tokens.cache.write;
}

// integrations/opencode/node_modules/@opencode/schema/dist/session-message.js
var ID13 = Schema27.String.check(Schema27.isStartingWith("msg_")).pipe(Schema27.brand("Session.Message.ID"), Schema27.annotate({ identifier: "Session.Message.ID" }), statics((schema) => ({
  create: () => schema.make("msg_" + ascending()),
  fromEvent: (eventID) => schema.make(eventID.replace(/^evt_/, "msg_"))
}))), Base = {
  id: ID13,
  metadata: Schema27.Record(Schema27.String, Schema27.Unknown).pipe(optional),
  time: Schema27.Struct({ created: DateTimeUtcFromMillis })
}, Native = Schema27.Record(Schema27.String, Schema27.Unknown).annotate({
  identifier: "Session.Message.Native"
}), AgentSelected = Schema27.Struct({
  ...Base,
  type: Schema27.tag("agent-switched"),
  agent: exports_agent.ID,
  previous: exports_agent.ID.pipe(optional)
}).annotate({ identifier: "Session.Message.AgentSelected" }), ModelSelected = Schema27.Struct({
  ...Base,
  type: Schema27.tag("model-switched"),
  model: exports_model.Ref,
  previous: exports_model.Ref.pipe(optional)
}).annotate({ identifier: "Session.Message.ModelSelected" }), LocationSwitched = Schema27.Struct({
  ...Base,
  type: Schema27.tag("location-switched"),
  location: exports_location.Ref,
  projectID: exports_project.ID.pipe(optional),
  subpath: RelativePath.pipe(optional),
  previous: Schema27.Struct({
    location: exports_location.Ref,
    projectID: exports_project.ID.pipe(optional),
    subpath: RelativePath.pipe(optional)
  }).pipe(optional)
}).annotate({ identifier: "Session.Message.LocationSwitched" }), User = Schema27.Struct({
  ...Base,
  text: Prompt.fields.text,
  files: Prompt.fields.files,
  agents: Prompt.fields.agents,
  skills: Prompt.fields.skills,
  type: Schema27.tag("user")
}).annotate({ identifier: "Session.Message.User" }), MediaUser = Schema27.Struct({
  ...User.fields,
  files: Schema27.Array(Attachment).pipe(optional)
}).annotate({ identifier: "Session.Message.MediaUser" }), Synthetic = Schema27.Struct({
  ...Base,
  text: Schema27.String,
  description: Schema27.String.pipe(optional),
  type: Schema27.tag("synthetic")
}).annotate({ identifier: "Session.Message.Synthetic" }), System = Schema27.Struct({
  ...Base,
  type: Schema27.tag("system"),
  text: Schema27.String,
  description: Schema27.String.pipe(optional)
}).annotate({ identifier: "Session.Message.System" }), Skill = Schema27.Struct({
  ...Base,
  type: Schema27.tag("skill"),
  skill: exports_skill.ID,
  name: exports_skill.Name,
  text: Schema27.String
}).annotate({ identifier: "Session.Message.Skill" }), Shell = Schema27.Struct({
  ...Base,
  type: Schema27.tag("shell"),
  shellID: exports_shell.ID,
  command: Schema27.String,
  status: exports_shell.Status,
  exit: Schema27.Number.pipe(optional),
  output: exports_shell.Output.pipe(optional),
  time: Schema27.Struct({
    created: DateTimeUtcFromMillis,
    completed: DateTimeUtcFromMillis.pipe(optional)
  })
}).annotate({ identifier: "Session.Message.Shell" }), ToolStateStreaming = Schema27.Struct({
  status: Schema27.tag("streaming"),
  input: Schema27.String
}).annotate({ identifier: "Session.Message.ToolState.Streaming" }), ToolStateRunning = Schema27.Struct({
  status: Schema27.tag("running"),
  input: Schema27.Record(Schema27.String, Schema27.Unknown),
  metadata: Schema27.Record(Schema27.String, Schema27.Json)
}).annotate({ identifier: "Session.Message.ToolState.Running" }), ToolStateCompleted = Schema27.Struct({
  status: Schema27.tag("completed"),
  input: Schema27.Record(Schema27.String, Schema27.Unknown),
  content: Schema27.NonEmptyArray(Content),
  metadata: Schema27.Record(Schema27.String, Schema27.Json).pipe(optional)
}).annotate({ identifier: "Session.Message.ToolState.Completed" }), ToolStateError = Schema27.Struct({
  status: Schema27.tag("error"),
  input: Schema27.Record(Schema27.String, Schema27.Unknown),
  error: exports_session_error.Error,
  content: Schema27.NonEmptyArray(Content).pipe(optional),
  metadata: Schema27.Record(Schema27.String, Schema27.Json).pipe(optional)
}).annotate({ identifier: "Session.Message.ToolState.Error" }), ToolState = Schema27.Union([ToolStateStreaming, ToolStateRunning, ToolStateCompleted, ToolStateError]).pipe(Schema27.toTaggedUnion("status")), AssistantTool = Schema27.Struct({
  type: Schema27.tag("tool"),
  id: Schema27.String,
  name: Schema27.String,
  executed: Schema27.Boolean.pipe(optional),
  native: Native.pipe(optional),
  state: ToolState,
  time: Schema27.Struct({
    created: DateTimeUtcFromMillis,
    ran: DateTimeUtcFromMillis.pipe(optional),
    completed: DateTimeUtcFromMillis.pipe(optional)
  })
}).annotate({ identifier: "Session.Message.Assistant.Tool" }), AssistantText = Schema27.Struct({
  type: Schema27.tag("text"),
  text: Schema27.String,
  native: Native.pipe(optional)
}).annotate({ identifier: "Session.Message.Assistant.Text" }), AssistantReasoning = Schema27.Struct({
  type: Schema27.tag("reasoning"),
  text: Schema27.String,
  native: Native.pipe(optional),
  time: Schema27.Struct({
    created: DateTimeUtcFromMillis,
    completed: DateTimeUtcFromMillis.pipe(optional)
  }).pipe(optional)
}).annotate({ identifier: "Session.Message.Assistant.Reasoning" }), AssistantContent = Schema27.Union([AssistantText, AssistantReasoning, AssistantTool]).pipe(Schema27.toTaggedUnion("type")), AssistantContentEncoded = Schema27.toEncoded(AssistantContent).annotate({
  identifier: "Session.Message.AssistantContent.Encoded"
}), AssistantRetry = Schema27.Struct({
  attempt: PositiveInt,
  at: DateTimeUtcFromMillis,
  error: exports_session_error.Error
}).annotate({ identifier: "Session.Message.Assistant.Retry" }), Assistant = Schema27.Struct({
  ...Base,
  type: Schema27.tag("assistant"),
  agent: exports_agent.ID,
  model: exports_model.Ref,
  content: AssistantContent.pipe(Schema27.Array),
  snapshot: Schema27.Struct({
    start: exports_snapshot.ID.pipe(optional),
    end: exports_snapshot.ID.pipe(optional),
    files: Schema27.Array(RelativePath).pipe(optional)
  }).pipe(optional),
  finish: FinishReason.pipe(optional),
  rawFinish: Schema27.String.pipe(optional),
  native: Native.pipe(optional),
  cost: exports_money.USD.pipe(optional),
  tokens: exports_token_usage.Info.pipe(optional),
  error: exports_session_error.Error.pipe(optional),
  retry: AssistantRetry.pipe(optional),
  time: Schema27.Struct({
    created: DateTimeUtcFromMillis,
    streamed: DateTimeUtcFromMillis.pipe(optional),
    completed: DateTimeUtcFromMillis.pipe(optional)
  })
}).annotate({ identifier: "Session.Message.Assistant" }), CompactionBase = { type: Schema27.tag("compaction"), ...Base }, CompactionUsage = {
  cost: exports_money.USD.pipe(optional),
  tokens: exports_token_usage.Info.pipe(optional)
}, CompactionRunning = Schema27.Struct({
  ...CompactionBase,
  status: Schema27.tag("running"),
  reason: Schema27.Literals(["auto", "manual"]),
  summary: Schema27.String
}).annotate({ identifier: "Session.Message.Compaction.Running" }), CompactionCompleted = Schema27.Struct({
  ...CompactionBase,
  status: Schema27.tag("completed"),
  reason: Schema27.Literals(["auto", "manual"]),
  model: exports_model.Ref.pipe(optional),
  native: Native.pipe(optional),
  summary: Schema27.String,
  ...CompactionUsage
}).annotate({ identifier: "Session.Message.Compaction.Completed" }), CompactionFailed = Schema27.Struct({
  ...CompactionBase,
  status: Schema27.tag("failed"),
  reason: Schema27.Literals(["auto", "manual"]),
  error: exports_session_error.Error,
  ...CompactionUsage
}).annotate({ identifier: "Session.Message.Compaction.Failed" }), Compaction2 = Schema27.Union([CompactionRunning, CompactionCompleted, CompactionFailed]).pipe(Schema27.toTaggedUnion("status"), Schema27.annotate({ identifier: "Session.Message.Compaction" })), Idle = Schema27.Struct({
  ...Base,
  type: Schema27.tag("idle"),
  outcome: Schema27.Literals(["succeeded", "failed", "interrupted"]),
  error: exports_session_error.Error.pipe(optional)
}).annotate({ identifier: "Session.Message.Idle" }), Info12 = Schema27.Union([
  AgentSelected,
  ModelSelected,
  LocationSwitched,
  User,
  Synthetic,
  System,
  Skill,
  Shell,
  Assistant,
  Compaction2,
  Idle
]).annotate({ identifier: "Session.Message.Info" }), MediaInfo = Schema27.Union([
  AgentSelected,
  ModelSelected,
  LocationSwitched,
  MediaUser,
  Synthetic,
  System,
  Skill,
  Shell,
  Assistant,
  Compaction2,
  Idle
]).annotate({ identifier: "Session.Message.MediaInfo" });

// integrations/opencode/appencode.ts
import { Cause, Effect as Effect4, Fiber, Schedule, Schema as Schema28, Semaphore } from "effect";
import { lstat as lstat3, realpath as realpath3, stat } from "fs/promises";
import { dirname as dirname3, isAbsolute as isAbsolute3, join as join3, relative as relative2, resolve as resolve3, sep } from "path";

// integrations/opencode/refusal.ts
var REFUSAL_CLASSES = [
  "storage",
  "untrusted_log",
  "policy_unavailable",
  "policy_moved",
  "legacy_family",
  "inventory_refused",
  "engine_invariant",
  "contended",
  "resolution_diverged",
  "annotation_refused",
  "undeclared_tool",
  "undeclared_spawn",
  "peer_message",
  "principal",
  "outstanding_call",
  "call_id_reused",
  "trajectory_ended",
  "trajectory",
  "dispatch",
  "remedy",
  "spawn_not_taken",
  "spawn"
];
function isRefusalClass(value) {
  return REFUSAL_CLASSES.includes(value);
}
function refusal(reply) {
  let { class: refused, detail } = reply;
  return isRefusalClass(refused) && typeof detail === "string" ? { class: refused, detail } : void 0;
}

// integrations/opencode/safebash.ts
import { spawn, spawnSync } from "child_process";
import { lstat, mkdir, mkdtemp, readFile, realpath, writeFile } from "fs/promises";
import { tmpdir } from "os";
import { basename, dirname, join, relative, resolve, isAbsolute } from "path";
var DESCRIPTION = `Run one shell command in a sandbox with bash -c. The command reads only the files, directories and Unix sockets in input_files, plus system directories such as /usr and /etc, the directories on PATH and installed toolchains. It writes only the files and directories in output_files, and connects only to the hosts in network_hosts (example.com, *.example.com, or host:port). Relative paths resolve from the project directory.

Every other file in the workspace, the home directory and /tmp is hidden, so ls, find and globs there show only what the call declares; an empty listing does not mean a file is missing. To list a directory, declare it in input_files. A credential file such as .env inside a declared directory stays hidden unless the call names it or git tracks it. Declare a socket such as /var/run/docker.sock in input_files to reach the service behind it. $TMPDIR is a private directory for this call alone, removed when it ends.

An output file that exists is read as well, so a command may rewrite it, as npm install does package.json, but not replace, rename or remove it: write through it, as sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f does in place of sed -i. An output written with a trailing / is a directory: the command reads what it holds and writes under it, as a build directory, node_modules/ or a cache needs. Declare a file's directory with a trailing / only for a tool that always replaces the file, such as cargo add.

Name only the hosts the command needs: a host other than a package registry's can lower the session's trust. go get through the Go proxy needs proxy.golang.org and sum.golang.org, not github.com; cargo needs index.crates.io and static.crates.io. A call that names a Go, cargo or npm registry keeps what it downloads in that package manager's cache, which every later command reads without declaring it.

The sandbox keeps a repository's config and hooks read-only, even in a declared directory, so git config, git remote add and the upstream that git push -u sets fail: leave those to the person, in their own terminal.`, HOST = /^(\*\.)?[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*(:[1-9][0-9]{0,4})?$/;
function strings(value, field) {
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string" && item.length > 0))
    throw Error(`[appencode] appa_safebash requires ${field} as an array of non-empty strings.`);
  return value;
}
var temporary = (path) => /^\$(TMPDIR|\{TMPDIR\})(\/|$)/.test(path);
async function canonical(path, directory) {
  let existing = resolve(directory, path), absent = [];
  for (;; )
    try {
      return join(await realpath(existing), ...absent.reverse());
    } catch {
      if (dirname(existing) === existing)
        throw Error(`[appencode] ${JSON.stringify(path)} names no file.`);
      absent.push(basename(existing)), existing = dirname(existing);
    }
}
async function writable(path, what) {
  let existing = await lstat(path).catch((error) => {
    if (error?.code === "ENOENT")
      return;
    throw error;
  });
  if (existing && (!existing.isFile() || existing.nlink !== 1))
    throw Error(`[appencode] ${what} ${JSON.stringify(path)} must be absent or a singly linked regular file; write a directory with a trailing /.`);
}
async function checkedWrite(input, directory) {
  if (!input || typeof input !== "object" || !("path" in input) || typeof input.path !== "string")
    throw Error("[appencode] write and edit require a path.");
  let parent = await realpath(dirname(resolve(directory, input.path))).catch(() => {
    throw Error(`[appencode] path ${JSON.stringify(input.path)} has no existing parent directory.`);
  }), path = join(parent, basename(input.path));
  return await writable(path, "path"), { ...input, path };
}
async function checkedSafeBash(input, directory) {
  if (!input || typeof input !== "object" || !("command" in input) || typeof input.command !== "string" || !input.command.trim())
    throw Error("[appencode] appa_safebash requires a command.");
  let fields = input, unknown = Object.keys(fields).filter((key) => !["command", "input_files", "output_files", "network_hosts"].includes(key));
  if (unknown.length)
    throw Error(`[appencode] appa_safebash does not accept ${unknown.join(", ")}.`);
  let inputs = [];
  for (let path of strings(fields.input_files, "input_files"))
    inputs.push(temporary(path) ? path : await canonical(path, directory));
  let outputs = [];
  for (let path of strings(fields.output_files, "output_files")) {
    if (temporary(path)) {
      outputs.push(path);
      continue;
    }
    let resolved = await canonical(path, directory), folder = path.endsWith("/") || (await lstat(resolved).catch(() => {
      return;
    }))?.isDirectory();
    if (!folder)
      await writable(resolved, "appa_safebash output");
    outputs.push(folder && resolved !== "/" ? `${resolved}/` : resolved);
  }
  let hosts = strings(fields.network_hosts, "network_hosts").map((host) => host.toLowerCase()), invalid = hosts.find((host) => !HOST.test(host));
  if (invalid !== void 0)
    throw Error(`[appencode] appa_safebash network host ${JSON.stringify(invalid)} is not a hostname, *.suffix, or host:port.`);
  return { command: input.command, input_files: [...new Set(inputs)], output_files: [...new Set(outputs)], network_hosts: [...new Set(hosts)] };
}
async function sandboxRuntime(configured) {
  let data = process.env.APPA_DATA_DIR || join(process.env.XDG_DATA_HOME || join(process.env.HOME || "/", ".local/share"), "appa"), installed = join(data, "sandbox-runtime/node_modules/.bin/srt"), executable = configured || Bun.which("srt") || await lstat(installed).then(() => installed, () => {
    return;
  });
  if (!executable)
    throw Error("[appencode] Shell commands run in the Anthropic Sandbox Runtime, which is not installed. Install it with npm install -g @anthropic-ai/sandbox-runtime, or set APPA_OPENCODE_SRT.");
  let real = await realpath(executable), root = resolve(dirname(real), "..");
  if (JSON.parse(await readFile(join(root, "package.json"), "utf8").catch(() => "{}")).name !== "@anthropic-ai/sandbox-runtime")
    throw Error("[appencode] APPA_OPENCODE_SRT must name the srt executable of @anthropic-ai/sandbox-runtime.");
  return { executable: real, package: root };
}
function confinement(request, cwd, appa) {
  let answer = spawnSync(appa || "appa", ["file-tracker", "sandbox"], { cwd, input: JSON.stringify(request), encoding: "utf8", timeout: 30000 });
  if (answer.error || answer.status !== 0) {
    let reason = String(answer.stderr || answer.error?.message || "no answer").trim().replace(/^appa file-tracker: /, "");
    throw Error(`[appencode] The command was not run: ${reason}`);
  }
  return JSON.parse(answer.stdout);
}
function settings(confined, srt, sockets) {
  return JSON.stringify({
    filesystem: {
      ...confined.filesystem,
      allowRead: [...confined.filesystem.allowRead, srt.package, sockets],
      denyWrite: [...confined.filesystem.denyWrite, srt.package]
    },
    network: { allowedDomains: confined.network.allowedDomains, deniedDomains: [] }
  });
}
function environment(confined, sockets) {
  let env = {};
  for (let [name, value] of Object.entries(process.env))
    if (value !== void 0 && !confined.hiddenEnvironment.includes(name))
      env[name] = value;
  return { ...env, TMPDIR: sockets };
}
var HINTS = [
  {
    partial: !0,
    command: /(^|[\s;&|(`])(ls|find|tree|fd|du|git\s+(status|diff|ls-files|add|clean|stash))\b|\*/,
    text: "[appa] This command saw only its declared input_files: undeclared files were hidden, so a listing or git status here is incomplete. To see a directory, declare it in input_files."
  },
  {
    output: /^(?!.*config file).*(read-only file system|EROFS)/im,
    text: "[appa] A write to a path not in output_files fails as a read-only file system. Declare the path in output_files, or its directory with a trailing /, such as node_modules/.vite/ or .git/."
  },
  {
    output: /could not (lock|write) config file/,
    text: "[appa] The sandbox keeps .git/config read-only, even in a declared directory: a setting there, such as core.hooksPath or core.fsmonitor, would run code outside the sandbox. So git config and git remote add fail, and git push -u pushes but sets no upstream. Tell the person the git command to run in their own terminal."
  },
  {
    output: /^(?!.*config file).*(Device or resource busy|EBUSY)/m,
    text: `[appa] A declared output file can be rewritten but not replaced, renamed or removed, so a tool that writes a temporary file and renames it over the output fails. Write through the file instead: sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f in place of sed -i. For a tool that always replaces the file, such as cargo add, declare the file's directory with a trailing / in output_files.`
  },
  {
    output: /not a git repository/,
    text: "[appa] Git found no repository because the repository's root, the directory that holds .git, is not in input_files. Declare the root in input_files, and its .git/ in output_files if git writes."
  },
  {
    output: /cannot lock ref|unable to create directory for .*\.git\/|Unable to create '[^']*\.lock'|unable to (create temporary|write new index)/,
    text: "[appa] Git writes branches, its index and new objects under .git. When the repository's .git/ is not in output_files, it is read-only to this command, so git commit, git checkout -b and git switch fail. Declare it with a trailing /, such as <repository>/.git/, beside the files the command changes."
  },
  {
    output: /^c\S{9}\s.*\s1,\s+3\s/m,
    text: "[appa] An entry listed as character device 1, 3, such as .bashrc or .vscode, is /dev/null, which the sandbox mounts where a protected file does not exist so that the command cannot create it. There is no such file."
  }
];
function hints(call, cwd, printed) {
  let whole = call.input_files.some((declared) => {
    let within = relative(declared, cwd);
    return !within.startsWith("..") && !isAbsolute(within);
  });
  return HINTS.filter((hint) => hint.partial && whole ? !1 : hint.command ? hint.command.test(call.command) : hint.output.test(printed)).map((hint) => hint.text);
}
async function runSafeBash(call, cwd, runtime, signal) {
  let report = await new Promise((done, fail) => {
    let child = spawn(runtime.appa || "appa", ["file-tracker", "run"], { cwd, detached: !0, stdio: ["pipe", "pipe", "pipe"] }), stdout = "", stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    }), child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    let stop = () => {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {}
    };
    signal.addEventListener("abort", stop, { once: !0 }), child.on("error", () => fail(Error("[appencode] appa_safebash could not start appa file-tracker run. Put appa on PATH, or set APPA_FILE_TRACKER."))), child.on("close", (code) => {
      if (signal.removeEventListener("abort", stop), signal.aborted)
        return fail(Error("[appencode] appa_safebash was cancelled."));
      if (code !== 0)
        return fail(Error(`[appencode] The command was not run: ${stderr.trim().replace(/^appa file-tracker: /, "")}`));
      try {
        done(JSON.parse(stdout));
      } catch {
        fail(Error("[appencode] appa file-tracker run sent an unreadable result."));
      }
    }), child.stdin.end(JSON.stringify(call));
  });
  return [report, ...hints(call, cwd, report)].join(`
`);
}
async function typedShell(command, cwd, runtime) {
  if (process.platform !== "linux")
    throw Error("[appencode] Typed shell commands run sandboxed on Linux only.");
  let srt = await sandboxRuntime(runtime.srt), confined = confinement({ command, typed: !0 }, cwd, runtime.appa), run = await mkdtemp(join(tmpdir(), "appencode-typed-")), sockets = join(run, "tmp");
  await mkdir(sockets, { mode: 448 }), await writeFile(join(run, "settings.json"), settings(confined, srt, sockets), { mode: 384 });
  let quoted = (text) => `'${text.replace(/'/g, "'\\''")}'`;
  return {
    shell: "/bin/sh",
    env: environment(confined, sockets),
    command: `${quoted(srt.executable)} --settings ${quoted(join(run, "settings.json"))} -c ${quoted(`${confined.prelude}${command}`)}; status=$?; rm -rf ${quoted(run)} ${quoted(confined.tmpdir)}; exit $status`
  };
}

// integrations/opencode/setup.ts
import { createHash } from "crypto";
import { lstat as lstat2, mkdir as mkdir2, readFile as readFile2, readdir, realpath as realpath2, rename, rm, symlink, writeFile as writeFile2 } from "fs/promises";
import { dirname as dirname2, isAbsolute as isAbsolute2, join as join2, resolve as resolve2 } from "path";
var checkout = resolve2(import.meta.dir, "../.."), batteries = join2(checkout, "marketplace/batteries"), digest = (text) => createHash("sha256").update(text).digest("hex");
async function configuration(config, binary) {
  if (!isAbsolute2(config) || !isAbsolute2(binary))
    throw Error("Setup requires absolute config and appa paths.");
  if (!(await lstat2(config)).isFile())
    throw Error("Setup config must be a regular file, not a symlink.");
  let target = process.env.APPA_BATTERY_TARGET ?? "single-user";
  if (!["single-user", "multi-user"].includes(target))
    throw Error("APPA_BATTERY_TARGET must be single-user or multi-user.");
  let store = process.env.APPA_OPENCODE_BATTERIES || join2(dirname2(config), "batteries"), catalog = await Promise.all((await readdir(store)).sort().filter((name) => !name.includes(".")).map(async (name) => {
    let manifest = Bun.TOML.parse(await readFile2(join2(store, name, "appa-package.toml"), "utf8")), targets = manifest.battery.targets, policy = targets?.[target]?.policy;
    return {
      name,
      description: manifest.description,
      hosts: manifest.battery.hosts,
      namespaces: manifest.battery.namespaces ?? [],
      path: `batteries/${name}/appa.toml`,
      supported: !targets || policy !== void 0,
      target_policy: policy === manifest.battery.policy ? void 0 : policy
    };
  })).then((items) => items.filter((item) => item.hosts.includes("opencode") && item.supported)), publishing = !1;
  async function describe(path, tools = []) {
    let process2 = Bun.spawn([
      binary,
      "describe",
      "--adapter",
      "opencode",
      "--config",
      path,
      "--check",
      ...tools.length ? ["--session-tools", tools.join(",")] : []
    ], { stdout: "pipe", stderr: "pipe" }), [stdout, stderr, code] = await Promise.all([new Response(process2.stdout).text(), new Response(process2.stderr).text(), process2.exited]);
    if (code !== 0)
      throw Error(`Policy validation refused: ${stderr}
${stdout}`);
    return stdout;
  }
  async function documents(path, found = /* @__PURE__ */ new Map) {
    if (found.has(path))
      return found;
    let text = await readFile2(path, "utf8");
    found.set(path, text);
    let includes = Bun.TOML.parse(text).include ?? [];
    for (let include of includes)
      await documents(resolve2(dirname2(path), include), found);
    return found;
  }
  return async (input, tools, servers = []) => {
    let inventory2 = {
      scope: "observed",
      servers,
      note: "Tools are a live registry observation, not proof of complete MCP discovery or policy coverage. Connected servers can still have registry updates pending. Disabled servers are excluded; resolve pending, failed, or needs_auth servers and inspect again before proposing policy."
    };
    switch (input.operation) {
      case "inspect": {
        let policy = await readFile2(config, "utf8");
        return JSON.stringify({
          config,
          policy,
          digest: digest(policy),
          deployment_target: target,
          description: await describe(config, tools.map((tool) => tool.canonical)),
          documents: Object.fromEntries(await documents(config)),
          tools,
          inventory: inventory2,
          batteries: catalog
        });
      }
      case "battery": {
        let battery = catalog.find((item) => item.name === input.battery);
        if (!battery)
          throw Error("Choose a battery from the inspected catalog.");
        let contracts = process.env.APPA_OPENCODE_SETUP_CONTRACTS;
        if (!contracts || !isAbsolute2(contracts))
          throw Error("Setup requires the contract reference path from the operator launcher.");
        return JSON.stringify({
          ...battery,
          deployment_target: target,
          files: Object.fromEntries(await Promise.all(["appa-package.toml", "appa.toml", "README.md", ...battery.target_policy ? [battery.target_policy] : []].map(async (name) => [name, await readFile2(join2(store, battery.name, name), "utf8")]))),
          contracts: await readFile2(contracts, "utf8")
        });
      }
      case "publish": {
        if (!input.policy || !input.expected_digest)
          throw Error("Publish requires the approved complete policy and inspected digest.");
        let unavailable = servers.filter((server) => server.status !== "connected" && server.status !== "disabled");
        if (unavailable.length)
          throw Error(`MCP discovery is unresolved: ${unavailable.map((server) => `${server.name} (${server.status})`).join(", ")}. Resolve these connections and inspect again before publishing.`);
        if (publishing)
          throw Error("Another publish is in progress; inspect again.");
        publishing = !0;
        let candidate = join2(dirname2(config), `.appa-setup-${crypto.randomUUID()}.toml`);
        try {
          let previous = await readFile2(config, "utf8");
          if (digest(previous) !== input.expected_digest)
            throw Error("Policy changed; inspect and propose again.");
          if (previous === input.policy)
            return "Policy unchanged. No new session is needed.";
          await writeFile2(candidate, input.policy, { mode: 384, flag: "wx" });
          let description = await describe(candidate, tools.map((tool) => tool.canonical));
          if (digest(await readFile2(config, "utf8")) !== input.expected_digest)
            throw Error("Policy changed during validation; inspect and propose again.");
          return await rename(candidate, config), JSON.stringify({
            state: "saved",
            description,
            inventory: inventory2,
            next: "Close setup. Restart the protected runtime and start a fresh OpenCode session. Existing sessions use the saved policy for their next tool call."
          });
        } finally {
          publishing = !1, await rm(candidate, { force: !0 });
        }
      }
    }
  };
}

// integrations/opencode/appencode.ts
var UNMEDIATED = ["shell", "patch", "grep"];
function text(decision, field) {
  let value = decision[field];
  if (typeof value !== "string")
    throw Error(`[appencode] Runtime reply is missing ${field}.`);
  return value;
}
function failure(error) {
  return new exports_tool.Error({ message: error instanceof Error && error.message.startsWith("[appencode]") ? error.message : "[appencode] Runtime unavailable or invalid reply. Check APPA_OPENCODE_RUNTIME_URL and the runtime logs." });
}
var appencode_default = Plugin.define({
  id: "appa.appencode",
  effect: Effect4.fn(function* (ctx) {
    let base = process.env.APPA_OPENCODE_RUNTIME_URL || ctx.options?.runtimeURL || "http://127.0.0.1:8789", sandbox = { appa: process.env.APPA_FILE_TRACKER || ctx.options?.appa, srt: process.env.APPA_OPENCODE_SRT || ctx.options?.srt }, setupConfig = process.env.APPA_OPENCODE_SETUP_CONFIG, setup = setupConfig ? yield* Effect4.tryPromise(() => configuration(setupConfig, process.env.APPA_OPENCODE_SETUP_BINARY || "")).pipe(Effect4.orDie) : void 0, setupGuide, pending = /* @__PURE__ */ new Map, children = /* @__PURE__ */ new Map, initialPrompts = /* @__PURE__ */ new Set, launches = /* @__PURE__ */ new Map, tasks = /* @__PURE__ */ new Map, disposed = !1;
    async function post(event, actor, fields = {}) {
      if (event === "tool_call" && actor.child && !children.has(actor.child))
        throw Error("[appencode] Child has no live delegation.");
      let response2 = await fetch(`${base.replace(/\/$/, "")}/hook`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          protocol: 1,
          adapter: "opencode",
          event,
          root_id: actor.root,
          ...actor.child ? { child_id: actor.child } : {},
          ...fields
        }),
        signal: AbortSignal.timeout(30000),
        redirect: "error"
      }), reply = await response2.json(), invalid = Error("[appencode] Invalid runtime reply. Check --adapter opencode and the runtime logs.");
      if (!reply || reply.protocol !== 1 || typeof reply.decision !== "string")
        throw invalid;
      if (reply.decision === "refuse") {
        let refused = refusal(reply);
        if (!refused || response2.status !== 409)
          throw invalid;
        throw Error(`[appencode] ${refused.detail} (${refused.class})`);
      }
      if (!response2.ok)
        throw Error(`[appencode] Runtime returned HTTP ${response2.status}.`);
      return reply;
    }
    async function open(sessionID) {
      let session = await Effect4.runPromise(ctx.session.get({ sessionID }));
      if (session.fork)
        throw Error("[appencode] Forked sessions are not supported. Continue the protected root instead.");
      if (session.parentID) {
        let child = children.get(sessionID);
        if (!child || child.root !== session.parentID)
          throw Error("[appencode] Child has no live delegation. Resume and alternate launches are refused.");
        return await childHistory(sessionID), { actor: { root: child.root, child: sessionID }, context: child.context };
      }
      let actor = { root: sessionID }, started = await post("session_start", actor);
      if (started.decision !== "ack" && started.decision !== "context")
        throw Error("[appencode] Unexpected session-start decision.");
      return { actor, context: started.decision === "context" ? text(started, "text") : "" };
    }
    async function childHistory(sessionID) {
      let child = children.get(sessionID);
      if (!child)
        throw Error("[appencode] Child has no live delegation.");
      let history = await Effect4.runPromise(ctx.session.context({ sessionID }));
      for (let message of history)
        switch (message.type) {
          case "user":
            if (message.id !== child.promptID || message.text !== child.prompt)
              throw Error("[appencode] Additional child input is not supported.");
            break;
          case "assistant":
          case "system":
            break;
          case "idle":
            if (message.error)
              throw Error("[appencode] Child execution failed; its return is withheld.");
            break;
          default:
            throw Error("[appencode] Child synthetic input, compaction and alternate work are not supported.");
        }
      return history;
    }
    async function childAnswer(sessionID) {
      let answer = [...await childHistory(sessionID)].reverse().find((message) => message.type === "assistant");
      if (!answer || answer.type !== "assistant" || answer.error || !answer.time.completed || answer.finish !== "stop")
        throw Error("[appencode] Child did not complete a final answer; its return is withheld.");
      return answer.content.filter((part) => part.type === "text").map((part) => part.text).join(`
`);
    }
    function revoke(child) {
      children.delete(child), initialPrompts.delete(child);
    }
    async function ownedTask(handle, sessionID) {
      await open(sessionID);
      let task = tasks.get(handle);
      if (disposed || children.has(sessionID) || !task || task.root !== sessionID)
        throw Error("[appencode] Unknown task or task not owned by this root.");
      return task;
    }
    let cancelTask = (task) => task.transition.withPermits(1)(Effect4.gen(function* () {
      if (task.state.status === "admitted")
        return "Task already returned; cancellation cannot retract its return.";
      if (task.state.status === "cancelled" && task.state.settled)
        return "Task cancelled.";
      if (task.state = { status: "cancelled", settled: !1 }, revoke(task.child), yield* ctx.session.interrupt({ sessionID: task.child, resume: !1 }), yield* ctx.session.wait({ sessionID: task.child }), (yield* Effect4.tryPromise({ try: () => post("child_end", { root: task.root }, { child_id: task.child }), catch: failure })).decision !== "ack")
        throw Error("[appencode] Cancellation is not confirmed; retry cancellation.");
      return task.state = { status: "cancelled", settled: !0 }, "Task cancelled.";
    }).pipe(Effect4.uninterruptible)), admit = (task) => task.transition.withPermits(1)(Effect4.tryPromise({ try: async () => {
      if (disposed || tasks.get(task.handle) !== task)
        throw Error("[appencode] Task ownership expired.");
      switch (task.state.status) {
        case "admitted":
          return task.state.value;
        case "cancelled":
          throw Error("[appencode] Task cancelled; its return is withheld.");
        case "failed":
          throw Error(task.state.message);
        case "active":
          break;
      }
      if (task.state.candidate === void 0)
        try {
          task.state.candidate = await childAnswer(task.child);
        } catch {
          throw task.state = { status: "failed", message: "[appencode] Child did not complete a valid final answer; its return is withheld." }, revoke(task.child), await post("child_end", { root: task.root }, { child_id: task.child }).catch(() => Effect4.runPromise(Effect4.logWarning("[appencode] Failed child settlement is unconfirmed."))), Error("[appencode] Child did not complete a valid final answer; its return is withheld.");
        }
      let returned = await post("child_end", { root: task.root }, { child_id: task.child, value: task.state.candidate });
      if (returned.decision === "child_return") {
        if (task.state.candidate = text(returned, "value"), (await post("child_end", { root: task.root }, { child_id: task.child, value: task.state.candidate })).decision !== "ack")
          throw Error("[appencode] Replacement return was not admitted.");
      } else if (returned.decision !== "ack")
        throw Error("[appencode] Child return was not admitted; retry collection or cancel the task.");
      let value = task.state.candidate;
      return task.state = { status: "admitted", value, reported: !1 }, revoke(task.child), value;
    }, catch: failure }).pipe(Effect4.uninterruptible)), watch = (task) => Effect4.gen(function* () {
      if (yield* ctx.session.wait({ sessionID: task.child }), yield* admit(task).pipe(Effect4.retry({ times: 2, schedule: Schedule.spaced("1 second"), while: () => task.state.status === "active" }), Effect4.result), disposed || tasks.get(task.handle) !== task || task.state.status === "cancelled")
        return;
      let notice;
      if (task.state.status === "admitted") {
        if (task.state.reported)
          return;
        task.state.reported = !0, notice = `[appencode] Background task ${task.handle} finished. Its APPA-admitted return follows.

${task.state.value}`;
      } else if (task.state.status === "failed")
        notice = `[appencode] Background task ${task.handle} ended without a valid final answer; its return is withheld.`;
      else
        notice = `[appencode] Background task ${task.handle} finished, but APPA did not admit its return. Call appa_collect to retry or appa_cancel to stop it.`;
      yield* ctx.session.synthetic({ sessionID: task.root, id: task.notice, text: notice, delivery: "queue", resume: !0 }).pipe(Effect4.retry({ times: 2, schedule: Schedule.spaced("1 second") }));
    }).pipe(Effect4.catchCause((cause) => Cause.hasInterrupts(cause) ? Effect4.void : Effect4.logWarning("[appencode] Background result delivery failed; appa_collect still returns it.")));
    yield* Effect4.addFinalizer(() => Effect4.gen(function* () {
      disposed = !0;
      let owned = [...tasks.values()];
      tasks.clear();
      for (let task of owned)
        revoke(task.child);
      for (let task of owned) {
        if (task.watcher)
          yield* Effect4.forkDetach(Fiber.interrupt(task.watcher));
        if (task.state.status === "admitted")
          continue;
        if (yield* ctx.session.interrupt({ sessionID: task.child, resume: !1 }).pipe(Effect4.catchCause(() => Effect4.logWarning("[appencode] Background cleanup is unconfirmed."))), task.state.status !== "cancelled" || !task.state.settled)
          yield* Effect4.logWarning("[appencode] Background ownership expired; execution settlement is unconfirmed.");
      }
    }));
    async function inventory2() {
      return (await Effect4.runPromise(ctx.tool.list())).map((tool) => {
        let namespace = tool.options?.namespace;
        if (namespace?.includes("__") || !namespace && tool.id.startsWith("mcp__"))
          throw Error("[appencode] Tool registry contains an ambiguous wire spelling.");
        return { name: tool.id, tool: namespace ? `mcp__${namespace}__${tool.name}` : tool.id };
      });
    }
    let remedyReady = Effect4.gen(function* () {
      for (;; ) {
        if ((yield* ctx.tool.list()).some((tool) => tool.options?.namespace === "appa" && tool.name === "execute_remedy_plan" && tool.options.codemode === !1))
          return;
        let server = (yield* ctx.mcp.list()).data.find((server2) => server2.name === "appa");
        if (!server || !["pending", "connected"].includes(server.status.status))
          return yield* Effect4.fail(new exports_tool.Error({ message: `[appencode] APPA MCP is ${server?.status.status ?? "not configured"}. Connect the appa server before protected work.` }));
        yield* Effect4.sleep("50 millis");
      }
    }).pipe(Effect4.timeoutOrElse({ duration: "10 seconds", orElse: () => Effect4.fail(new exports_tool.Error({ message: "[appencode] APPA remedy tool was not registered within 10 seconds. Check the appa MCP connection and retry the prompt." })) }), Effect4.catch((error) => Effect4.fail(failure(error)))), setupDiscovery = Effect4.gen(function* () {
      for (;; ) {
        if (!(yield* ctx.mcp.list()).data.some((server) => server.status.status === "pending"))
          return;
        yield* Effect4.sleep("50 millis");
      }
    }).pipe(Effect4.timeoutOption("10 seconds"));
    function feedback(value, tools) {
      for (let tool of [...tools].sort((a, b) => b.tool.length - a.tool.length))
        value = value.split(tool.tool).join(tool.name);
      return value.split("mcp__appa__execute_remedy_plan").join("appa_execute_remedy_plan");
    }
    yield* ctx.tool.transform((editor) => {
      editor.remove("subagent");
      for (let name of UNMEDIATED)
        editor.remove(name);
      if (editor.add({
        name: "appa_safebash",
        options: { codemode: !1 },
        description: DESCRIPTION,
        input: Schema28.Struct({
          command: Schema28.String,
          input_files: Schema28.Array(Schema28.String),
          output_files: Schema28.Array(Schema28.String),
          network_hosts: Schema28.Array(Schema28.String)
        }),
        output: Schema28.String,
        execute: (input, call) => Effect4.gen(function* () {
          let approved = pending.get(`${call.sessionID}:${call.id}`);
          if (!approved || JSON.stringify(approved.proposal.arguments) !== JSON.stringify(input))
            return yield* Effect4.fail(new exports_tool.Error({ message: "[appencode] appa_safebash runs only APPA-released calls." }));
          let output = yield* Effect4.tryPromise({
            try: (signal) => runSafeBash(input, ctx.location.directory, sandbox, signal),
            catch: (error) => new exports_tool.Error({ message: error instanceof Error && error.message.startsWith("[appencode]") ? error.message : "[appencode] appa_safebash sandbox failed to start." })
          });
          return { output, content: output };
        })
      }), setup)
        editor.add({
          name: "appa_setup",
          options: { codemode: !1 },
          description: "Inspect AppenCode policy and live tools, read a catalog battery, or publish a complete approved policy. Publish only after a later operator message approves your proposal. Configuration access only; no coding or shell execution.",
          input: Schema28.Struct({
            operation: Schema28.Literals(["inspect", "battery", "publish"]),
            battery: Schema28.optionalKey(Schema28.String),
            policy: Schema28.optionalKey(Schema28.String),
            expected_digest: Schema28.optionalKey(Schema28.String)
          }),
          output: Schema28.String,
          execute: (input, call) => Effect4.gen(function* () {
            let session = yield* ctx.session.get({ sessionID: call.sessionID });
            if (session.parentID || session.fork)
              return yield* Effect4.fail(new exports_tool.Error({ message: "Setup requires a fresh root operator session." }));
            if (input.operation !== "battery")
              yield* setupDiscovery;
            let servers = yield* ctx.mcp.list(), tools = yield* ctx.tool.list(), output = yield* Effect4.tryPromise({
              try: () => setup(input, tools.map((tool) => ({
                name: tool.id,
                description: tool.description,
                canonical: tool.options?.namespace === "appa" && tool.name === "execute_remedy_plan" ? "appa/execute_remedy_plan" : tool.options?.namespace ? `mcp/${tool.options.namespace}/${tool.name}` : `host/opencode/${tool.name}`
              })), servers.data.map((server) => ({ name: server.name, status: server.status.status }))),
              catch: (error) => new exports_tool.Error({ message: error instanceof Error ? error.message : "AppenCode setup failed." })
            });
            return { output, content: output, metadata: { truncated: !1 } };
          }).pipe(Effect4.catch((error) => Effect4.fail(error instanceof exports_tool.Error ? error : failure(error))))
        });
      editor.add({
        name: "appa_subagent",
        options: { codemode: !1 },
        description: "Delegate to a fresh subagent through APPA. Declare its return using the offered remedy, then retry. Set background: true to receive an owned task handle and keep working; its checked final text arrives later as a message naming the handle, and its return then narrows this session. Set deliver: false as well to receive it only through appa_collect, so it narrows this session only when collected. Use appa_collect to wait for it now, or appa_cancel to stop it. Default: foreground. APPA authorizes delegation; native subagent permissions do not apply. No resume or nested delegation.",
        input: Schema28.Struct({
          agent: Schema28.String,
          description: Schema28.String,
          prompt: Schema28.String,
          model: Schema28.optionalKey(Schema28.String),
          sessionID: Schema28.optionalKey(Schema28.String),
          background: Schema28.optionalKey(Schema28.Boolean),
          deliver: Schema28.optionalKey(Schema28.Boolean)
        }),
        output: Schema28.String,
        execute: (input, call) => Effect4.gen(function* () {
          if (children.has(call.sessionID) || input.sessionID !== void 0)
            throw Error("[appencode] Only fresh, one-level delegation is supported.");
          if (input.deliver !== void 0 && input.background !== !0)
            throw Error("[appencode] deliver applies only to background delegation.");
          yield* Effect4.tryPromise({ try: () => open(call.sessionID), catch: failure });
          let parent = yield* ctx.session.get({ sessionID: call.sessionID }), agent = (yield* ctx.agent.list()).data.find((agent2) => agent2.id === input.agent || agent2.name === input.agent);
          if (!agent || agent.mode === "primary" || agent.hidden)
            throw Error("[appencode] Requested subagent is unavailable.");
          let model = agent.model ?? parent.model;
          if (input.model !== void 0) {
            let requested = exports_model.Ref.parse(input.model), selected = (yield* ctx.model.list()).data.find((item) => item.providerID === requested.providerID && item.id === requested.id);
            if (!selected || requested.variant !== void 0 && !selected.variants.some((item) => item.id === requested.variant))
              throw Error("[appencode] Requested model is unavailable.");
            model = requested;
          }
          let proposal = { tool: "appa_subagent", arguments: structuredClone(input), call_id: call.id }, tools = yield* Effect4.tryPromise({ try: inventory2, catch: failure }), runtime = (event, fields = {}) => Effect4.tryPromise({ try: () => post(event, { root: call.sessionID }, fields), catch: failure }), releaseFields = { ...proposal, cwd: ctx.location.directory, inventory: { tools } }, release = "unattempted", settled = !1, transferred = !1, backgroundReport, childID, cleanup = Effect4.gen(function* () {
            if (transferred)
              return;
            if (childID)
              children.delete(childID), initialPrompts.delete(childID), yield* ctx.session.interrupt({ sessionID: childID, resume: !1 }).pipe(Effect4.catchCause(() => Effect4.void)), yield* ctx.session.wait({ sessionID: childID }).pipe(Effect4.catchCause(() => Effect4.void));
            if (settled)
              return;
            if (release === "uncertain")
              release = (yield* runtime("tool_call", releaseFields)).decision === "allow_call" ? "approved" : "unattempted";
            if (release === "approved") {
              if (childID)
                yield* runtime("child_end", { child_id: childID }).pipe(Effect4.catchCause(() => Effect4.void));
              settled = (yield* runtime("spawn_result", backgroundReport ?? { ...proposal, spawned_id: childID, outcome: { status: "indeterminate" } })).decision === "ack";
            }
          }).pipe(Effect4.catchCause(() => input.background === !0 ? Effect4.logWarning("[appencode] Background launch cleanup is unconfirmed.") : Effect4.void)), launch = launches.get(call.sessionID);
          if (!launch)
            launch = Semaphore.makeUnsafe(1), launches.set(call.sessionID, launch);
          return yield* Effect4.gen(function* () {
            let child = yield* launch.withPermits(1)(Effect4.gen(function* () {
              release = "uncertain";
              let decision = yield* runtime("tool_call", releaseFields);
              if (release = decision.decision === "allow_call" ? "approved" : "unattempted", decision.decision !== "allow_call")
                throw Error(`[appencode] ${feedback(text(decision, decision.decision === "deny_call" ? "feedback" : "reason"), tools)}`);
              let binding = text(decision, "spawn_binding");
              if (!binding)
                throw Error("[appencode] Spawn approval has no binding.");
              let child2 = yield* ctx.session.create({
                parentID: call.sessionID,
                agent: exports_agent.ID.make(agent.id),
                model,
                title: input.description
              });
              if (childID = child2.id, child2.parentID !== call.sessionID || child2.fork)
                throw Error("[appencode] Invalid child lineage.");
              let started = yield* runtime("child_start", { child_id: child2.id, spawn_binding: binding });
              if (started.decision !== "ack" && started.decision !== "context")
                throw Error("[appencode] Unexpected child-start decision.");
              let owned = {
                id: child2.id,
                root: call.sessionID,
                promptID: exports_session_message.ID.create(),
                prompt: input.prompt,
                context: started.decision === "context" ? text(started, "text") : ""
              };
              return children.set(child2.id, owned), owned;
            }).pipe(Effect4.onError(() => cleanup), Effect4.uninterruptible));
            if (initialPrompts.add(child.id), yield* ctx.session.prompt({ sessionID: child.id, id: child.promptID, text: child.prompt }), input.background === !0)
              return yield* Effect4.gen(function* () {
                let task = crypto.randomUUID();
                if (backgroundReport = {
                  ...proposal,
                  spawned_id: child.id,
                  outcome: { status: "success", body: { output: task, content: task } }
                }, (yield* runtime("spawn_result", backgroundReport)).decision !== "ack")
                  throw Error("[appencode] Background launch was not confirmed.");
                if (disposed)
                  throw Error("[appencode] Plugin disposed during launch.");
                let owned = {
                  handle: task,
                  root: call.sessionID,
                  child: child.id,
                  proposal,
                  promptID: child.promptID,
                  notice: exports_session_message.ID.create(),
                  transition: Semaphore.makeUnsafe(1),
                  state: { status: "active" }
                };
                if (tasks.set(task, owned), input.deliver !== !1)
                  owned.watcher = yield* Effect4.forkDetach(watch(owned));
                return settled = !0, transferred = !0, { output: task, content: task };
              }).pipe(Effect4.uninterruptible);
            yield* ctx.session.wait({ sessionID: child.id });
            let value = yield* Effect4.tryPromise({ try: () => childAnswer(child.id), catch: failure }), returned = yield* runtime("child_end", { child_id: childID, value });
            if (returned.decision === "child_return") {
              if (value = text(returned, "value"), (yield* runtime("child_end", { child_id: childID, value })).decision !== "ack")
                throw Error("[appencode] Replacement return was not admitted.");
            } else if (returned.decision !== "ack")
              throw Error(`[appencode] ${feedback(text(returned, "reason"), tools)}`);
            if ((yield* runtime("spawn_result", {
              ...proposal,
              spawned_id: childID,
              value,
              outcome: { status: "success_without_body" }
            })).decision !== "ack")
              throw Error("[appencode] Foreground return was not confirmed.");
            return settled = !0, { output: value, content: value };
          }).pipe(Effect4.ensuring(cleanup));
        }).pipe(Effect4.catchCause((cause) => {
          if (Cause.hasInterrupts(cause))
            return Effect4.interrupt;
          return Effect4.fail(failure(Cause.squash(cause)));
        }))
      }), editor.add({
        name: "appa_collect",
        options: { codemode: !1 },
        description: "Wait for an owned background task and return only its APPA-admitted final text. Use it when you need the result before continuing, or for a task launched with deliver: false; otherwise it arrives as a message. Root-only. Repeated collection reuses the admitted result. Interrupting this wait does not cancel the task.",
        input: Schema28.Struct({ task: Schema28.String }),
        output: Schema28.String,
        execute: (input, call) => Effect4.gen(function* () {
          let task = yield* Effect4.tryPromise({ try: () => ownedTask(input.task, call.sessionID), catch: failure });
          if (task.state.status === "active")
            yield* ctx.session.wait({ sessionID: task.child });
          let value = yield* admit(task);
          if (task.state.status === "admitted")
            task.state.reported = !0;
          if (disposed || tasks.get(input.task) !== task)
            throw Error("[appencode] Task ownership expired.");
          return { output: value, content: value };
        }).pipe(Effect4.catchCause((cause) => Cause.hasInterrupts(cause) ? Effect4.interrupt : Effect4.fail(failure(Cause.squash(cause)))))
      }), editor.add({
        name: "appa_cancel",
        options: { codemode: !1 },
        description: "Revoke and stop an owned background task without returning child content. Root-only; safe to repeat. Cannot undo effects or retract an admitted return. Retry if cancellation is unconfirmed.",
        input: Schema28.Struct({ task: Schema28.String }),
        output: Schema28.String,
        execute: (input, call) => Effect4.gen(function* () {
          let task = yield* Effect4.tryPromise({ try: () => ownedTask(input.task, call.sessionID), catch: failure }), value = yield* cancelTask(task);
          return { output: value, content: value };
        }).pipe(Effect4.catchCause((cause) => Cause.hasInterrupts(cause) ? Effect4.interrupt : Effect4.fail(failure(Cause.squash(cause)))))
      });
      for (let tool of editor.list())
        editor.update(tool.id, (item) => {
          let { pinned: _pinned, ...options } = item.options ?? {};
          item.options = { ...options, codemode: !1 };
        });
    }), yield* ctx.permission.hook("evaluate", (event) => Effect4.sync(() => {
      if (event.action === "execute" || event.action === "subagent")
        event.effect = "deny", event.message = "Use APPA delegation, not Code Mode or native subagent returns.";
    })), yield* ctx.shell.hook("create.before", (event) => Effect4.promise(async () => {
      try {
        let root = await realpath3(ctx.location.directory), within = relative2(root, await realpath3(event.cwd));
        if (isAbsolute3(within) || within === ".." || within.startsWith(`..${sep}`))
          throw Error("[appencode] Shell commands run only in the project directory.");
        Object.assign(event, await typedShell(event.command, event.cwd, sandbox));
      } catch (error) {
        let reason = error instanceof Error && error.message.startsWith("[appencode]") ? error.message : "[appencode] The sandbox for shell commands failed to start.";
        event.shell = "/bin/sh", event.env = {}, event.command = `printf '%s\\n' '${reason.replace(/'/g, "'\\''")} Use appa_safebash.' >&2; exit 126`;
      }
    })), yield* ctx.session.hook("prompt", (event) => Effect4.tryPromise({
      try: async () => {
        if (setup && !setupGuide) {
          let path = process.env.APPA_OPENCODE_SETUP_GUIDE;
          if (!path || !isAbsolute3(path) || await realpath3(path) !== path)
            throw Error("[appencode] Setup requires a fixed canonical guide path from the operator launcher.");
          let file = await lstat3(path);
          if (!file.isFile())
            throw Error("[appencode] Setup guide must be a regular file.");
          let project = resolve3(ctx.location.project.directory);
          for (let root of [ctx.location.directory, ...project === dirname3(project) ? [] : [project]])
            for (let directory of [resolve3(root), await realpath3(root)]) {
              let within = relative2(directory, path);
              if (!isAbsolute3(within) && within !== ".." && !within.startsWith(`..${sep}`))
                throw Error("[appencode] Setup guide must be outside the OpenCode project and Location. Choose an external temporary directory.");
            }
          setupGuide = { path, dev: file.dev, ino: file.ino };
        }
        let session = await Effect4.runPromise(ctx.session.get({ sessionID: event.sessionID }));
        if (session.fork)
          throw Error("[appencode] Forked sessions are not supported.");
        if (!session.parentID)
          return;
        let child = children.get(event.sessionID);
        if (!child || !initialPrompts.delete(event.sessionID) || event.messageID !== child.promptID || event.prompt.text !== child.prompt || event.prompt.files?.length || event.prompt.agents?.length || event.prompt.skills?.length)
          throw Error("[appencode] Child prompts require an exact live delegation; steering and alternate launches are refused.");
      },
      catch: failure
    }).pipe(Effect4.andThen(setup ? Effect4.void : remedyReady), Effect4.tapError((error) => Effect4.logError(error.message)), Effect4.orDie));
    for (let kind of ["compaction", "generate", "title"])
      yield* ctx.session.hook(kind, (event) => Effect4.tryPromise({
        try: async () => {
          let session = await Effect4.runPromise(ctx.session.get({ sessionID: event.sessionID }));
          if (session.parentID || session.fork)
            throw Error("[appencode] Auxiliary child and fork requests are not supported.");
        },
        catch: failure
      }).pipe(Effect4.orDie));
    yield* ctx.session.hook("context", (event) => Effect4.tryPromise({
      try: async () => {
        if (setup && !setupGuide)
          throw Error("[appencode] Setup guide was not admitted; retry the prompt.");
        let { context } = await open(event.sessionID);
        delete event.tools.execute, delete event.tools.subagent;
        for (let name of UNMEDIATED)
          delete event.tools[name];
        if (children.has(event.sessionID))
          delete event.tools.appa_subagent, delete event.tools.appa_collect, delete event.tools.appa_cancel;
        if (setup) {
          for (let name of Object.keys(event.tools))
            if (name !== "appa_setup" && name !== "read")
              delete event.tools[name];
        }
        event.system.push({ type: "text", text: setupGuide ? `Read the setup guide first with native read at exactly ${JSON.stringify(setupGuide.path)}. Only that file is readable; other files and directory listings are unavailable. Then use appa_setup to inspect and propose. Wait for a later operator approval before publishing. No coding, shell, MCP or delegation calls are available in setup.` : context || "AppenCode checks direct tool flows through APPA. Use the offered remedy tool, then retry. Do not bypass refused flows." });
      },
      catch: failure
    }).pipe(Effect4.orDie)), yield* ctx.tool.hook("execute.before", (event) => Effect4.tryPromise({
      try: async () => {
        if (setup && !setupGuide)
          throw Error("[appencode] Setup guide was not admitted; retry the prompt.");
        if (setupGuide) {
          if (event.tool !== "appa_setup" && event.tool !== "read")
            throw Error("[appencode] Setup permits only appa_setup and the fixed guide read.");
          if (event.tool === "read") {
            if (!event.input || typeof event.input !== "object" || !("path" in event.input) || event.input.path !== setupGuide.path)
              throw Error("[appencode] Setup read is restricted to the fixed guide file.");
            let file = await lstat3(setupGuide.path);
            if (!file.isFile() || file.dev !== setupGuide.dev || file.ino !== setupGuide.ino || await realpath3(setupGuide.path) !== setupGuide.path)
              throw Error("[appencode] Setup guide was replaced; close and restart setup.");
          }
        }
        if (event.tool === "appa_subagent") {
          if (await open(event.sessionID), children.has(event.sessionID))
            throw Error("[appencode] Nested delegation is not supported.");
          return;
        }
        if (event.tool === "execute" || event.tool === "subagent")
          throw Error("[appencode] Code Mode and native subagents are refused. Use direct tools or appa_subagent.");
        if (UNMEDIATED.includes(event.tool))
          throw Error("[appencode] Unsandboxed shell, patch and grep tools are refused. Use appa_safebash, read, write or edit.");
        let { actor } = await open(event.sessionID);
        if (actor.child && (event.tool === "appa_collect" || event.tool === "appa_cancel"))
          throw Error("[appencode] Background task operations require the owning root.");
        let tools = await inventory2(), tool = tools.find((tool2) => tool2.name === event.tool);
        if (!tool)
          throw Error("[appencode] Tool is absent from the current registry.");
        if (event.tool === "read")
          event.input = await checkedRead(event.input, ctx.location.directory);
        if (event.tool === "write" || event.tool === "edit")
          event.input = await checkedWrite(event.input, ctx.location.directory);
        if (event.tool === "appa_safebash")
          event.input = await checkedSafeBash(event.input, ctx.location.directory);
        let proposal = { tool: tool.tool, arguments: structuredClone(event.input), call_id: event.id }, decision = await post("tool_call", actor, {
          ...proposal,
          cwd: ctx.location.directory,
          inventory: { tools }
        });
        switch (decision.decision) {
          case "allow_call":
          case "pass_control":
            pending.set(`${event.sessionID}:${event.id}`, { proposal, actor });
            return;
          case "deny_call":
            throw Error(`[appencode] ${feedback(text(decision, "feedback"), tools)}`);
          case "block":
            throw Error(`[appencode] ${feedback(text(decision, "reason"), tools)}`);
          default:
            throw Error("[appencode] Unexpected tool-call decision.");
        }
      },
      catch: failure
    })), yield* ctx.tool.hook("execute.after", (event) => Effect4.promise(async () => {
      if (event.tool === "appa_subagent")
        return;
      let key = `${event.sessionID}:${event.id}`, approved = pending.get(key);
      pending.delete(key);
      let replace = (value) => {
        if (event.status === "completed")
          event.result = { output: value, content: value };
        else
          event.error = new exports_tool.Error({ message: value });
      };
      try {
        if (!approved)
          throw Error("[appencode] No local approval for this result; it is withheld.");
        let { proposal, actor } = approved;
        if (JSON.stringify(proposal.arguments) !== JSON.stringify(event.input))
          throw Error("[appencode] Tool arguments changed after approval; the result is withheld.");
        let outcome = event.status === "error" ? { status: "failure", message: JSON.stringify(event.error) } : { status: "success", body: event.result }, decision = await post("tool_result", actor, { ...proposal, outcome });
        switch (decision.decision) {
          case "ack":
            return;
          case "deliver_value":
            replace(text(decision, "value"));
            return;
          case "replace_output":
            replace(feedback(text(decision, "output"), await inventory2()));
            return;
          case "block":
            replace(feedback(text(decision, "reason"), await inventory2()));
            return;
          default:
            throw Error("[appencode] Unexpected tool-result decision.");
        }
      } catch (error) {
        replace(`${failure(error).message} Tool result withheld.`);
      }
    }));
  })
});
async function checkedRead(input, directory) {
  if (!input || typeof input !== "object" || !("path" in input) || typeof input.path !== "string")
    throw Error("[appencode] Read requires a path.");
  let path = await realpath3(resolve3(directory, input.path)).catch(() => {
    throw Error("[appencode] Cannot resolve the read path. Check that it exists and is accessible; alternate filenames are refused in v1.");
  }), root = await realpath3(directory), within = relative2(root, path);
  if (path === root || !isAbsolute3(within) && within !== ".." && !within.startsWith(`..${sep}`)) {
    let current = (await stat(path)).isDirectory() ? path : dirname3(path);
    while (current !== root && current !== dirname3(current)) {
      let instruction = join3(current, "AGENTS.md");
      if (instruction !== path)
        try {
          throw await lstat3(instruction), Error("[appencode] This read would inject unchecked nearby AGENTS.md instructions. V1 refuses it.");
        } catch (error) {
          if (!error || typeof error !== "object" || !("code" in error) || error.code !== "ENOENT")
            throw error;
        }
      current = dirname3(current);
    }
  }
  return { ...input, path };
}
export {
  appencode_default as default
};
