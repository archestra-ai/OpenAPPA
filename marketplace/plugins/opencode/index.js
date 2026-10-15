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
      if (!id.startsWith("wrk"))
        throw Error(`ID ${id} does not start with wrk`);
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
  Settings: () => Settings2,
  Ref: () => Ref3,
  ReasoningField: () => ReasoningField,
  Overlays: () => Overlays2,
  Model: () => exports_model,
  MaxTokensField: () => MaxTokensField,
  Info: () => Info6,
  ID: () => ID6,
  Family: () => Family,
  Event: () => Event5,
  Cost: () => Cost,
  Compatibility: () => Compatibility,
  Capabilities: () => Capabilities
});
import { Schema as Schema14 } from "effect";

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
  Info: () => Info3,
  EnvInfo: () => EnvInfo,
  CredentialInfo: () => CredentialInfo,
  Connection: () => exports_connection
});
import { Schema as Schema9 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/credential.js
var exports_credential = {};
__export(exports_credential, {
  Value: () => Value2,
  OAuth: () => OAuth,
  Key: () => Key,
  ID: () => ID3,
  External: () => External,
  Event: () => Event2,
  Entry: () => Entry,
  Credential: () => exports_credential,
  CreateInput: () => CreateInput
});
import { Schema as Schema8 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/integration-id.js
import { Schema as Schema6 } from "effect";
var IntegrationID = Schema6.String.pipe(Schema6.brand("Integration.ID"), Schema6.annotate({ identifier: "Integration.ID" })), IntegrationMethodID = Schema6.String.pipe(Schema6.brand("Integration.MethodID"), Schema6.annotate({ identifier: "Integration.MethodID" }));

// integrations/opencode/node_modules/@opencode/schema/dist/form.js
var exports_form = {};
__export(exports_form, {
  When: () => When,
  Value: () => Value,
  StringField: () => StringField,
  State: () => State,
  Reply: () => Reply,
  Option: () => Option2,
  NumberField: () => NumberField,
  MultiselectField: () => MultiselectField,
  Metadata: () => Metadata,
  IntegerField: () => IntegerField,
  Info: () => Info2,
  ID: () => ID2,
  Form: () => exports_form,
  Fields: () => Fields,
  Field: () => Field,
  ExternalField: () => ExternalField,
  Event: () => Event,
  Detail: () => Detail,
  BooleanField: () => BooleanField,
  Answer: () => Answer
});
import { Schema as Schema7 } from "effect";
var IDSchema = Schema7.String.check(Schema7.isStartingWith("frm_")).pipe(Schema7.brand("Form.ID"), Schema7.annotate({ identifier: "Form.ID" })), ID2 = IDSchema.pipe(statics((schema) => ({ create: (id) => schema.make(id ?? "frm_" + ascending()) }))), Metadata = Schema7.Record(Schema7.String, Schema7.Unknown).annotate({ identifier: "Form.Metadata" }), Option2 = Schema7.Struct({
  value: Schema7.String,
  label: Schema7.String,
  description: Schema7.String.pipe(optional)
}).annotate({ identifier: "Form.Option" }), When = Schema7.Struct({
  key: Schema7.String,
  op: Schema7.Literals(["eq", "neq"]),
  value: Schema7.Union([Schema7.String, Schema7.Number, Schema7.Boolean])
}).annotate({ identifier: "Form.When" }), FieldBase = {
  key: Schema7.String,
  title: Schema7.String.pipe(optional),
  description: Schema7.String.pipe(optional),
  required: Schema7.Boolean.pipe(optional),
  hidden: Schema7.Boolean.pipe(optional).annotate({
    description: "Skip the interactive authentication prompt and use the default unless an answer is supplied"
  }),
  when: Schema7.Array(When).pipe(optional)
}, StringField = Schema7.Struct({
  ...FieldBase,
  type: Schema7.Literal("string"),
  format: Schema7.Literals(["email", "uri", "date", "date-time"]).pipe(optional),
  minLength: NonNegativeInt.pipe(optional),
  maxLength: NonNegativeInt.pipe(optional),
  pattern: Schema7.String.pipe(optional),
  placeholder: Schema7.String.pipe(optional),
  default: Schema7.String.pipe(optional),
  options: Schema7.Array(Option2).pipe(optional),
  custom: Schema7.Boolean.pipe(optional)
}).annotate({ identifier: "Form.StringField" }), NumberField = Schema7.Struct({
  ...FieldBase,
  type: Schema7.Literal("number"),
  minimum: Schema7.Number.pipe(optional),
  maximum: Schema7.Number.pipe(optional),
  default: Schema7.Number.pipe(optional)
}).annotate({ identifier: "Form.NumberField" }), IntegerField = Schema7.Struct({
  ...FieldBase,
  type: Schema7.Literal("integer"),
  minimum: Schema7.Number.pipe(optional),
  maximum: Schema7.Number.pipe(optional),
  default: Schema7.Number.pipe(optional)
}).annotate({ identifier: "Form.IntegerField" }), BooleanField = Schema7.Struct({
  ...FieldBase,
  type: Schema7.Literal("boolean"),
  default: Schema7.Boolean.pipe(optional)
}).annotate({ identifier: "Form.BooleanField" }), MultiselectField = Schema7.Struct({
  ...FieldBase,
  type: Schema7.Literal("multiselect"),
  options: Schema7.Array(Option2),
  minItems: NonNegativeInt.pipe(optional),
  maxItems: NonNegativeInt.pipe(optional),
  custom: Schema7.Boolean.pipe(optional),
  default: Schema7.Array(Schema7.String).pipe(optional)
}).annotate({ identifier: "Form.MultiselectField" }), ExternalField = Schema7.Struct({
  key: Schema7.String,
  type: Schema7.Literal("external"),
  url: Schema7.String,
  title: Schema7.String.pipe(optional),
  description: Schema7.String.pipe(optional)
}).annotate({ identifier: "Form.ExternalField" }), Field = Schema7.Union([
  StringField,
  NumberField,
  IntegerField,
  BooleanField,
  MultiselectField,
  ExternalField
]).pipe(Schema7.toTaggedUnion("type"), Schema7.annotate({ identifier: "Form.Field" })), Fields = Schema7.NonEmptyArray(Field).annotate({ identifier: "Form.Fields" }), InfoBase = {
  id: ID2,
  sessionID: Schema7.String,
  title: Schema7.String,
  metadata: Metadata.pipe(optional)
}, Info2 = Schema7.Struct({
  ...InfoBase,
  fields: Fields
}).annotate({ identifier: "Form.Info" }), Value = Schema7.Union([Schema7.String, Schema7.Number, Schema7.Boolean, Schema7.Array(Schema7.String)]).annotate({
  identifier: "Form.Value"
}), Answer = Schema7.Record(Schema7.String, Value).annotate({ identifier: "Form.Answer" }), State = Schema7.Union([
  Schema7.Struct({ status: Schema7.Literal("pending") }),
  Schema7.Struct({ status: Schema7.Literal("answered"), answer: Answer }),
  Schema7.Struct({ status: Schema7.Literal("cancelled"), message: Schema7.String.pipe(optional) })
]).pipe(Schema7.toTaggedUnion("status")).annotate({ identifier: "Form.State" }), Detail = Schema7.Struct({
  ...Info2.fields,
  state: State
}).annotate({ identifier: "Form.Detail" }), Reply = Schema7.Struct({
  answer: Answer
}).annotate({ identifier: "Form.Reply" }), Created = ephemeral({ type: "form.created", schema: { form: Info2 } }), Replied = ephemeral({ type: "form.replied", schema: { id: ID2, sessionID: Schema7.String, answer: Answer } }), Cancelled = ephemeral({ type: "form.cancelled", schema: { id: ID2, sessionID: Schema7.String } }), Event = { Created, Replied, Cancelled, Definitions: inventory(Created, Replied, Cancelled) };

// integrations/opencode/node_modules/@opencode/schema/dist/credential.js
var ID3 = Schema8.String.pipe(Schema8.brand("Credential.ID"), Schema8.annotate({ identifier: "Credential.ID" }), statics((schema) => ({ create: () => schema.make("cred_" + ascending()) }))), Updated = ephemeral({
  type: "credential.updated",
  schema: {}
}), Switched = ephemeral({
  type: "credential.switched",
  schema: { integrationID: IntegrationID, credentialID: Schema8.NullOr(ID3) }
}), Event2 = {
  Updated,
  Switched,
  Definitions: inventory(Updated, Switched)
}, OAuth = Schema8.Struct({
  type: Schema8.Literal("oauth"),
  methodID: IntegrationMethodID,
  refresh: Schema8.String,
  access: Schema8.String,
  expires: NonNegativeInt,
  metadata: optional(Schema8.Record(Schema8.String, Schema8.Unknown))
}).annotate({ identifier: "Credential.OAuth" }), Key = Schema8.Struct({
  type: Schema8.Literal("key"),
  key: Schema8.String,
  metadata: optional(Schema8.Record(Schema8.String, Schema8.Unknown)),
  configuration: optional(exports_form.Answer)
}).annotate({ identifier: "Credential.Key" }), External = Schema8.Struct({
  type: Schema8.Literal("external"),
  methodID: IntegrationMethodID,
  metadata: optional(Schema8.Record(Schema8.String, Schema8.Unknown))
}).annotate({ identifier: "Credential.External" }), Value2 = Schema8.Union([OAuth, Key, External]).pipe(Schema8.toTaggedUnion("type")).annotate({ identifier: "Credential.Value" }), Entry = Schema8.Struct({
  id: ID3,
  integrationID: IntegrationID,
  label: Schema8.String,
  active: Schema8.Boolean,
  value: Value2
}).annotate({ identifier: "Credential.Entry" }), CreateInput = Schema8.Struct({
  id: optional(ID3),
  integrationID: IntegrationID,
  label: optional(Schema8.String),
  value: Value2,
  activate: optional(Schema8.Boolean)
}).annotate({ identifier: "Credential.CreateInput" });

// integrations/opencode/node_modules/@opencode/schema/dist/connection.js
var Status = Schema9.Struct({
  status: Schema9.Literals(["needs_auth"]),
  message: Schema9.String,
  url: optional(Schema9.String)
}).annotate({ identifier: "Connection.Status" }), CredentialInfo = Schema9.Struct({
  type: Schema9.Literal("credential"),
  id: exports_credential.ID,
  label: Schema9.String,
  method: Schema9.Literals(["key", "oauth", "external"]),
  status: optional(Status)
}).annotate({ identifier: "Connection.CredentialInfo" }), EnvInfo = Schema9.Struct({
  type: Schema9.Literal("env"),
  name: Schema9.String,
  status: optional(Status)
}).annotate({ identifier: "Connection.EnvInfo" }), Info3 = Schema9.Union([CredentialInfo, EnvInfo]).pipe(Schema9.toTaggedUnion("type")).annotate({ identifier: "Connection.Info" });

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
}), CacheFormat = Schema11.Literals(["anthropic", "alibaba"]).annotate({ identifier: "Model.CacheFormat" }), Compatibility = Schema11.Struct({
  reasoningField: ReasoningField.pipe(optional),
  requireReasoning: Schema11.Boolean.pipe(optional),
  maxTokensField: MaxTokensField.pipe(optional),
  requireFinishReason: Schema11.Boolean.pipe(optional),
  requireAssistantAfterTool: Schema11.Boolean.pipe(optional),
  supportsPromptCacheKey: Schema11.Boolean.pipe(optional),
  cacheFormat: CacheFormat.pipe(optional),
  supportsThinkingBlockBinding: Schema11.Boolean.pipe(optional),
  supportsEffortUpdates: Schema11.Boolean.pipe(optional)
}).annotate({ identifier: "Model.Compatibility" }), ProviderCompatibility = Schema11.Struct(Struct2.omit(Compatibility.fields, ["supportsThinkingBlockBinding", "supportsEffortUpdates"])).annotate({ identifier: "Provider.Compatibility" });

// integrations/opencode/node_modules/@opencode/schema/dist/provider.js
var ID5 = Schema12.String.pipe(Schema12.brand("Provider.ID"), Schema12.annotate({ identifier: "Provider.ID" }), statics((schema) => ({
  opencode: schema.make("opencode"),
  anthropic: schema.make("anthropic"),
  openai: schema.make("openai"),
  google: schema.make("google"),
  googleVertex: schema.make("google-vertex"),
  githubCopilot: schema.make("github-copilot"),
  amazonBedrock: schema.make("amazon-bedrock"),
  azure: schema.make("azure"),
  openrouter: schema.make("openrouter"),
  mistral: schema.make("mistral"),
  gitlab: schema.make("gitlab")
}))), Updated3 = ephemeral({ type: "provider.updated", schema: {} }), Event4 = { Updated: Updated3, Definitions: inventory(Updated3) }, Package = Schema12.String, Activation = Schema12.Literals(["auto", "enabled", "disabled"]), Compaction = Schema12.Union([
  Schema12.Struct({ type: Schema12.Literal("summary") }),
  Schema12.Struct({ type: Schema12.Literal("native") })
]).pipe(Schema12.toTaggedUnion("type")).annotate({ identifier: "Provider.Compaction" }), Transport = Schema12.Literals(["http", "websocket"]).annotate({ identifier: "Provider.Transport" }), Encoding = Schema12.Literals(["zstd", "gzip", "none"]).annotate({ identifier: "Provider.Encoding" }), Settings = Schema12.StructWithRest(Schema12.Struct({
  timeout: Schema12.Union([Schema12.Finite, Schema12.Literal(!1)]).pipe(optional),
  headerTimeout: Schema12.Union([Schema12.Finite, Schema12.Literal(!1)]).pipe(optional),
  chunkTimeout: Schema12.Union([Schema12.Finite, Schema12.Literal(!1)]).pipe(optional),
  compaction: Compaction.pipe(optional),
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
var Family = Schema14.String.pipe(Schema14.brand("Model.Family")), Settings2 = Schema14.StructWithRest(Schema14.Struct({
  compaction: exports_provider.Compaction.pipe(optional),
  encoding: exports_provider.Encoding.pipe(optional)
}), [Schema14.Record(Schema14.String, Schema14.Any)]).annotate({ identifier: "Model.Settings" }), Overlays2 = {
  settings: Settings2.pipe(optional),
  headers: Schema14.Record(Schema14.String, Schema14.String).pipe(optional),
  body: Schema14.Record(Schema14.String, Schema14.Any).pipe(optional)
}, Capabilities = Schema14.Struct({
  tools: Schema14.Boolean,
  input: Schema14.Array(Schema14.String),
  output: Schema14.Array(Schema14.String)
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
  ...Overlays2
}).annotate({ identifier: "Model.Variant" }), Info6 = Schema14.Struct({
  id: ID6,
  modelID: ID6,
  providerID: exports_provider.ID,
  canonical: exports_provider.ID.pipe(optional),
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
var SessionID = Schema15.String.check(Schema15.isStartingWith("ses")).pipe(Schema15.brand("SessionID"), Schema15.annotate({ identifier: "Session.ID" }), statics((schema) => {
  let create2 = () => schema.make("ses_" + descending());
  return {
    create: create2,
    descending: (id) => id === void 0 ? create2() : schema.make(id)
  };
}));

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
  ProviderState: () => ProviderState,
  ModelSelected: () => ModelSelected,
  LocationSwitched: () => LocationSwitched,
  Info: () => Info13,
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
import { Schema as Schema28 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/session-provider-context.js
var exports_session_provider_context = {};
__export(exports_session_provider_context, {
  SessionProviderContext: () => exports_session_provider_context,
  Provenance: () => Provenance,
  Info: () => Info8
});
import { Schema as Schema18 } from "effect";
var Provenance = Schema18.Struct({
  providerID: exports_provider.ID,
  provider: Schema18.String,
  modelID: Schema18.String,
  route: Schema18.String,
  protocol: Schema18.String,
  endpoint: Schema18.String
}).annotate({ identifier: "Session.ProviderContext.Provenance" }), Info8 = Schema18.Struct({
  version: Schema18.Literal(1),
  provenance: Provenance,
  messages: Schema18.Json
}).annotate({ identifier: "Session.ProviderContext" });

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
import { Effect as Effect3, JsonSchema, Schema as Schema19 } from "effect";
var CallID = Schema19.String.pipe(Schema19.brand("Tool.CallID"));

class Error2 extends Schema19.TaggedError()("Tool.Error", {
  message: Schema19.String,
  error: Schema19.optional(Schema19.Defect()),
  metadata: Schema19.optional(Schema19.Record(Schema19.String, Schema19.Unknown))
}) {
}
var TextContent = Schema19.Struct({
  type: Schema19.Literal("text"),
  text: Schema19.String
}).annotate({ identifier: "Tool.TextContent" }), FileContent = Schema19.Struct({
  type: Schema19.Literal("file"),
  uri: Schema19.String,
  mime: Schema19.String,
  name: Schema19.optional(Schema19.String)
}).annotate({ identifier: "Tool.FileContent" }), Content = Schema19.Union([TextContent, FileContent]).pipe(Schema19.toTaggedUnion("type")).annotate({ identifier: "Tool.Content" });

// integrations/opencode/node_modules/@opencode/schema/dist/project.js
var exports_project = {};
__export(exports_project, {
  Vcs: () => Vcs,
  UpdateInput: () => UpdateInput,
  Time: () => Time,
  Project: () => exports_project,
  Info: () => Info9,
  Icon: () => Icon,
  ID: () => ID9,
  Event: () => Event8,
  Current: () => Current,
  Commands: () => Commands
});
import { Schema as Schema20 } from "effect";
var ID9 = ProjectID, Vcs = Schema20.String.check(Schema20.isPattern(/^[a-z][a-z0-9._-]*$/u)).annotate({
  identifier: "Project.Vcs"
}), Current = Schema20.Struct({
  id: ID9,
  directory: AbsolutePath,
  canonical: AbsolutePath
}).annotate({ identifier: "Project.Current" }), Icon = Schema20.Struct({
  url: optional(Schema20.String),
  override: optional(Schema20.String),
  color: optional(Schema20.String)
}).annotate({ identifier: "Project.Icon" }), Commands = Schema20.Struct({
  start: optional(Schema20.String.annotate({ description: "Startup script to run when creating a new workspace (worktree)" }))
}).annotate({ identifier: "Project.Commands" }), Time = Schema20.Struct({
  created: NonNegativeInt,
  updated: NonNegativeInt,
  active: NonNegativeInt,
  archived: optional(NonNegativeInt)
}).annotate({ identifier: "Project.Time" }), Info9 = Schema20.Struct({
  id: ID9,
  canonical: AbsolutePath,
  vcs: optional(Vcs),
  name: optional(Schema20.String),
  icon: optional(Icon),
  commands: optional(Commands),
  time: Time,
  sandboxes: Schema20.Array(Schema20.String)
}).annotate({ identifier: "Project" }), UpdateInput = Schema20.Struct({
  projectID: ID9,
  canonical: optional(AbsolutePath),
  name: optional(Schema20.String),
  icon: optional(Icon),
  commands: optional(Commands)
}).annotate({ identifier: "Project.UpdateInput" }), Updated6 = ephemeral({ type: "project.updated", schema: Info9.fields }), Event8 = { Updated: Updated6, Definitions: inventory(Updated6) };

// integrations/opencode/node_modules/@opencode/schema/dist/prompt.js
import { Schema as Schema22 } from "effect";

// integrations/opencode/node_modules/@opencode/schema/dist/skill.js
var exports_skill = {};
__export(exports_skill, {
  UrlSource: () => UrlSource,
  Source: () => Source2,
  Skill: () => exports_skill,
  Name: () => Name2,
  Info: () => Info10,
  ID: () => ID10,
  Event: () => Event9,
  EmbeddedSource: () => EmbeddedSource,
  DirectorySource: () => DirectorySource
});
import { Schema as Schema21 } from "effect";
var ID10 = Schema21.String.pipe(Schema21.brand("Skill.ID"), Schema21.annotate({ identifier: "Skill.ID" })), Name2 = Schema21.String.pipe(Schema21.brand("Skill.Name"), Schema21.annotate({ identifier: "Skill.Name" })), DirectorySource = Schema21.Struct({
  type: Schema21.tag("directory"),
  path: AbsolutePath
}).annotate({ identifier: "Skill.DirectorySource" }), UrlSource = Schema21.Struct({
  type: Schema21.tag("url"),
  url: Schema21.String
}).annotate({ identifier: "Skill.UrlSource" }), Info10 = Schema21.Struct({
  id: ID10,
  name: Name2,
  description: Schema21.String.pipe(optional),
  autoinvoke: Schema21.Boolean.pipe(optional),
  path: AbsolutePath,
  content: Schema21.String
}).annotate({ identifier: "Skill.Info" }), Updated7 = ephemeral({ type: "skill.updated", schema: {} }), Event9 = { Updated: Updated7, Definitions: inventory(Updated7) }, EmbeddedSource = Schema21.Struct({
  type: Schema21.tag("embedded"),
  skill: Schema21.suspend(() => Info10)
}).annotate({ identifier: "Skill.EmbeddedSource" }), Source2 = Object.assign(Schema21.Union([DirectorySource, UrlSource, EmbeddedSource]).pipe(Schema21.toTaggedUnion("type"), Schema21.annotate({ identifier: "Skill.Source" })), {
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
var PromptMention = Schema22.Struct({
  start: Schema22.Finite,
  end: Schema22.Finite,
  text: Schema22.String
}).annotate({ identifier: "Prompt.Mention" }), FileSource = Schema22.Union([
  Schema22.Struct({ type: Schema22.Literal("inline") }),
  Schema22.Struct({ type: Schema22.Literal("uri"), uri: Schema22.String })
]).pipe(Schema22.toTaggedUnion("type")).annotate({ identifier: "Prompt.FileSource" }), Base64 = Schema22.String.check(Schema22.isPattern(/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u)).annotate({ identifier: "Prompt.Base64" }), FileAttachment = Schema22.Struct({
  data: Base64,
  mime: Schema22.String,
  source: FileSource,
  name: Schema22.String.pipe(optional),
  description: Schema22.String.pipe(optional),
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
}))), AgentAttachment = Schema22.Struct({
  name: Schema22.String,
  mention: PromptMention.pipe(optional)
}).annotate({ identifier: "Prompt.AgentAttachment" }), SkillAttachment = Schema22.Struct({
  id: exports_skill.ID,
  name: exports_skill.Name,
  text: Schema22.String.pipe(optional),
  mention: PromptMention.pipe(optional)
}).annotate({ identifier: "Prompt.SkillAttachment" }), Prompt = Schema22.Struct({
  text: Schema22.String,
  files: Schema22.Array(FileAttachment).pipe(optional),
  agents: Schema22.Array(AgentAttachment).pipe(optional),
  skills: Schema22.Array(SkillAttachment).pipe(optional)
}).annotate({ identifier: "Prompt" }).pipe(statics((schema) => ({
  equivalence: Schema22.toEquivalence(schema),
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
  Info: () => Info11,
  ID: () => ID11,
  Event: () => Event10,
  CreateInput: () => CreateInput2
});
import { Schema as Schema23 } from "effect";
var IDSchema2 = Schema23.String.check(Schema23.isStartingWith("sh_")).pipe(Schema23.brand("Shell.ID"), Schema23.annotate({ identifier: "Shell.ID" })), ID11 = IDSchema2.pipe(statics((schema) => {
  let create2 = () => schema.make("sh_" + ascending());
  return {
    create: create2,
    ascending: (id) => id === void 0 ? create2() : schema.make(id)
  };
})), Status2 = Schema23.Literals(["running", "exited", "timeout", "killed"]), Time2 = Schema23.Struct({
  started: Schema23.Finite.annotate({ description: "Start time in milliseconds since the Unix epoch" }),
  completed: optional(Schema23.Finite).annotate({
    description: "Completion time in milliseconds since the Unix epoch"
  })
}), Metadata2 = Schema23.Record(Schema23.String, Schema23.Unknown), Info11 = Schema23.Struct({
  id: ID11,
  status: Status2,
  command: Schema23.String,
  cwd: Schema23.String,
  shell: Schema23.String,
  file: Schema23.String,
  pid: optional(NonNegativeInt),
  exit: optional(Schema23.Finite),
  signal: optional(Schema23.String),
  metadata: Metadata2,
  time: Time2
}).annotate({ identifier: "Shell.Info" }), Created2 = ephemeral({ type: "shell.created", schema: { info: Info11 } }), Exited = ephemeral({ type: "shell.exited", schema: { id: ID11, exit: optional(Schema23.Finite), status: Status2 } }), Deleted = ephemeral({ type: "shell.deleted", schema: { id: ID11 } }), Event10 = { Created: Created2, Exited, Deleted, Definitions: inventory(Created2, Exited, Deleted) }, CreateInput2 = Schema23.Struct({
  command: Schema23.String,
  cwd: optional(Schema23.String),
  timeout: optional(NonNegativeInt),
  metadata: optional(Metadata2)
}), OutputInput = Schema23.Struct({
  cursor: optional(NonNegativeInt),
  limit: optional(NonNegativeInt)
}), Output = Schema23.Struct({
  output: Schema23.String,
  cursor: NonNegativeInt,
  size: NonNegativeInt,
  truncated: Schema23.Boolean
});

// integrations/opencode/node_modules/@opencode/schema/dist/llm.js
import { Schema as Schema24 } from "effect";
var FinishReason = Schema24.Literals(["stop", "length", "tool-calls", "content-filter", "error", "unknown"]);

// integrations/opencode/node_modules/@opencode/schema/dist/session-error.js
var exports_session_error = {};
__export(exports_session_error, {
  SessionError: () => exports_session_error,
  Error: () => Error3
});
import { Schema as Schema25 } from "effect";
var Error3 = Schema25.Struct({
  type: Schema25.String,
  message: Schema25.String,
  status: Schema25.Int.check(Schema25.isBetween({ minimum: 100, maximum: 599 })).pipe(optional),
  response: Schema25.Struct({ body: Schema25.String }).pipe(optional)
}).annotate({ identifier: "Session.StructuredError" });

// integrations/opencode/node_modules/@opencode/schema/dist/snapshot.js
var exports_snapshot = {};
__export(exports_snapshot, {
  Snapshot: () => exports_snapshot,
  ID: () => ID12
});
import { Schema as Schema26 } from "effect";
var ID12 = Schema26.String.pipe(Schema26.brand("Snapshot.ID"), Schema26.annotate({ identifier: "Snapshot.ID" }));

// integrations/opencode/node_modules/@opencode/schema/dist/token-usage.js
var exports_token_usage = {};
__export(exports_token_usage, {
  total: () => total,
  TokenUsage: () => exports_token_usage,
  Info: () => Info12
});
import { Schema as Schema27 } from "effect";
var Info12 = Schema27.Struct({
  input: Schema27.Finite,
  output: Schema27.Finite,
  reasoning: Schema27.Finite,
  cache: Schema27.Struct({
    read: Schema27.Finite,
    write: Schema27.Finite
  })
}).annotate({ identifier: "TokenUsage.Info" });
function total(tokens) {
  return tokens.input + tokens.output + tokens.reasoning + tokens.cache.read + tokens.cache.write;
}

// integrations/opencode/node_modules/@opencode/schema/dist/session-message.js
var ID13 = Schema28.String.check(Schema28.isStartingWith("msg_")).pipe(Schema28.brand("Session.Message.ID"), Schema28.annotate({ identifier: "Session.Message.ID" }), statics((schema) => ({
  create: () => schema.make("msg_" + ascending()),
  fromEvent: (eventID) => schema.make(eventID.replace(/^evt_/, "msg_"))
}))), Base = {
  id: ID13,
  metadata: Schema28.Record(Schema28.String, Schema28.Unknown).pipe(optional),
  time: Schema28.Struct({ created: DateTimeUtcFromMillis })
}, ProviderState = Schema28.Record(Schema28.String, Schema28.Unknown).annotate({
  identifier: "Session.Message.ProviderState"
}), AgentSelected = Schema28.Struct({
  ...Base,
  type: Schema28.tag("agent-switched"),
  agent: exports_agent.ID,
  previous: exports_agent.ID.pipe(optional)
}).annotate({ identifier: "Session.Message.AgentSelected" }), ModelSelected = Schema28.Struct({
  ...Base,
  type: Schema28.tag("model-switched"),
  model: exports_model.Ref,
  previous: exports_model.Ref.pipe(optional)
}).annotate({ identifier: "Session.Message.ModelSelected" }), LocationSwitched = Schema28.Struct({
  ...Base,
  type: Schema28.tag("location-switched"),
  location: exports_location.Ref,
  projectID: exports_project.ID.pipe(optional),
  subpath: RelativePath.pipe(optional),
  previous: Schema28.Struct({
    location: exports_location.Ref,
    projectID: exports_project.ID.pipe(optional),
    subpath: RelativePath.pipe(optional)
  }).pipe(optional)
}).annotate({ identifier: "Session.Message.LocationSwitched" }), User = Schema28.Struct({
  ...Base,
  text: Prompt.fields.text,
  files: Prompt.fields.files,
  agents: Prompt.fields.agents,
  skills: Prompt.fields.skills,
  type: Schema28.tag("user")
}).annotate({ identifier: "Session.Message.User" }), Synthetic = Schema28.Struct({
  ...Base,
  text: Schema28.String,
  description: Schema28.String.pipe(optional),
  type: Schema28.tag("synthetic")
}).annotate({ identifier: "Session.Message.Synthetic" }), System = Schema28.Struct({
  ...Base,
  type: Schema28.tag("system"),
  text: Schema28.String,
  description: Schema28.String.pipe(optional)
}).annotate({ identifier: "Session.Message.System" }), Skill = Schema28.Struct({
  ...Base,
  type: Schema28.tag("skill"),
  skill: exports_skill.ID,
  name: exports_skill.Name,
  text: Schema28.String
}).annotate({ identifier: "Session.Message.Skill" }), Shell = Schema28.Struct({
  ...Base,
  type: Schema28.tag("shell"),
  shellID: exports_shell.ID,
  command: Schema28.String,
  status: exports_shell.Status,
  exit: Schema28.Number.pipe(optional),
  output: exports_shell.Output.pipe(optional),
  time: Schema28.Struct({
    created: DateTimeUtcFromMillis,
    completed: DateTimeUtcFromMillis.pipe(optional)
  })
}).annotate({ identifier: "Session.Message.Shell" }), ToolStateStreaming = Schema28.Struct({
  status: Schema28.tag("streaming"),
  input: Schema28.String
}).annotate({ identifier: "Session.Message.ToolState.Streaming" }), ToolStateRunning = Schema28.Struct({
  status: Schema28.tag("running"),
  input: Schema28.Record(Schema28.String, Schema28.Unknown),
  metadata: Schema28.Record(Schema28.String, Schema28.Json)
}).annotate({ identifier: "Session.Message.ToolState.Running" }), ToolStateCompleted = Schema28.Struct({
  status: Schema28.tag("completed"),
  input: Schema28.Record(Schema28.String, Schema28.Unknown),
  content: Schema28.NonEmptyArray(Content),
  metadata: Schema28.Record(Schema28.String, Schema28.Json).pipe(optional)
}).annotate({ identifier: "Session.Message.ToolState.Completed" }), ToolStateError = Schema28.Struct({
  status: Schema28.tag("error"),
  input: Schema28.Record(Schema28.String, Schema28.Unknown),
  error: exports_session_error.Error,
  content: Schema28.NonEmptyArray(Content).pipe(optional),
  metadata: Schema28.Record(Schema28.String, Schema28.Json).pipe(optional)
}).annotate({ identifier: "Session.Message.ToolState.Error" }), ToolState = Schema28.Union([ToolStateStreaming, ToolStateRunning, ToolStateCompleted, ToolStateError]).pipe(Schema28.toTaggedUnion("status")), AssistantTool = Schema28.Struct({
  type: Schema28.tag("tool"),
  id: Schema28.String,
  name: Schema28.String,
  executed: Schema28.Boolean.pipe(optional),
  providerState: ProviderState.pipe(optional),
  providerResultState: ProviderState.pipe(optional),
  state: ToolState,
  time: Schema28.Struct({
    created: DateTimeUtcFromMillis,
    ran: DateTimeUtcFromMillis.pipe(optional),
    completed: DateTimeUtcFromMillis.pipe(optional)
  })
}).annotate({ identifier: "Session.Message.Assistant.Tool" }), AssistantText = Schema28.Struct({
  type: Schema28.tag("text"),
  text: Schema28.String,
  state: ProviderState.pipe(optional)
}).annotate({ identifier: "Session.Message.Assistant.Text" }), AssistantReasoning = Schema28.Struct({
  type: Schema28.tag("reasoning"),
  text: Schema28.String,
  state: ProviderState.pipe(optional),
  time: Schema28.Struct({
    created: DateTimeUtcFromMillis,
    completed: DateTimeUtcFromMillis.pipe(optional)
  }).pipe(optional)
}).annotate({ identifier: "Session.Message.Assistant.Reasoning" }), AssistantContent = Schema28.Union([AssistantText, AssistantReasoning, AssistantTool]).pipe(Schema28.toTaggedUnion("type")), AssistantContentEncoded = Schema28.toEncoded(AssistantContent).annotate({
  identifier: "Session.Message.AssistantContent.Encoded"
}), AssistantRetry = Schema28.Struct({
  attempt: PositiveInt,
  at: DateTimeUtcFromMillis,
  error: exports_session_error.Error
}).annotate({ identifier: "Session.Message.Assistant.Retry" }), Assistant = Schema28.Struct({
  ...Base,
  type: Schema28.tag("assistant"),
  agent: exports_agent.ID,
  model: exports_model.Ref,
  content: AssistantContent.pipe(Schema28.Array),
  snapshot: Schema28.Struct({
    start: exports_snapshot.ID.pipe(optional),
    end: exports_snapshot.ID.pipe(optional),
    files: Schema28.Array(RelativePath).pipe(optional)
  }).pipe(optional),
  finish: FinishReason.pipe(optional),
  rawFinish: Schema28.String.pipe(optional),
  providerState: ProviderState.pipe(optional),
  cost: exports_money.USD.pipe(optional),
  tokens: exports_token_usage.Info.pipe(optional),
  error: exports_session_error.Error.pipe(optional),
  retry: AssistantRetry.pipe(optional),
  time: Schema28.Struct({
    created: DateTimeUtcFromMillis,
    streamed: DateTimeUtcFromMillis.pipe(optional),
    completed: DateTimeUtcFromMillis.pipe(optional)
  })
}).annotate({ identifier: "Session.Message.Assistant" }), CompactionBase = { type: Schema28.tag("compaction"), ...Base }, CompactionUsage = {
  cost: exports_money.USD.pipe(optional),
  tokens: exports_token_usage.Info.pipe(optional)
}, CompactionRunning = Schema28.Struct({
  ...CompactionBase,
  status: Schema28.tag("running"),
  reason: Schema28.Literals(["auto", "manual"]),
  summary: Schema28.String,
  recent: Schema28.String
}).annotate({ identifier: "Session.Message.Compaction.Running" }), CompactionCompleted = Schema28.Struct({
  ...CompactionBase,
  status: Schema28.tag("completed"),
  reason: Schema28.Literals(["auto", "manual"]),
  model: exports_model.Ref.pipe(optional),
  providerState: ProviderState.pipe(optional),
  summary: Schema28.String,
  recent: Schema28.String,
  providerContext: exports_session_provider_context.Info.pipe(optional),
  ...CompactionUsage
}).annotate({ identifier: "Session.Message.Compaction.Completed" }), CompactionFailed = Schema28.Struct({
  ...CompactionBase,
  status: Schema28.tag("failed"),
  reason: Schema28.Literals(["auto", "manual"]),
  error: exports_session_error.Error,
  ...CompactionUsage
}).annotate({ identifier: "Session.Message.Compaction.Failed" }), Compaction2 = Schema28.Union([CompactionRunning, CompactionCompleted, CompactionFailed]).pipe(Schema28.toTaggedUnion("status"), Schema28.annotate({ identifier: "Session.Message.Compaction" })), Idle = Schema28.Struct({
  ...Base,
  type: Schema28.tag("idle"),
  outcome: Schema28.Literals(["succeeded", "failed", "interrupted"]),
  error: exports_session_error.Error.pipe(optional)
}).annotate({ identifier: "Session.Message.Idle" }), Info13 = Schema28.Union([
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
]).annotate({ identifier: "Session.Message.Info" });

// integrations/opencode/appencode.ts
import { Cause, Effect as Effect4, Schema as Schema29, Semaphore } from "effect";
import { lstat as lstat2, realpath as realpath2, stat } from "fs/promises";
import { dirname as dirname2, isAbsolute as isAbsolute2, join as join2, relative, resolve as resolve2, sep } from "path";

// integrations/opencode/setup.ts
import { createHash } from "crypto";
import { lstat, mkdir, readFile, readdir, realpath, rename, rm, symlink, writeFile } from "fs/promises";
import { dirname, isAbsolute, join, resolve } from "path";
import { existsSync } from "fs";
var checkout = resolve(import.meta.dir, "../.."), batteries = join(checkout, "marketplace/batteries"), assets = existsSync(join(import.meta.dir, "guide/SKILL.md")) ? import.meta.dir : void 0, digest = (text) => createHash("sha256").update(text).digest("hex");
async function guide() {
  let root = assets ? join(assets, "guide") : join(checkout, "integrations/appa-guide");
  return (await Promise.all(["SKILL.md", "references/core.md", "references/opencode.md"].map((path) => readFile(join(root, path), "utf8")))).join(`

`);
}
async function configuration(config, binary) {
  if (!isAbsolute(config) || !isAbsolute(binary))
    throw Error("Setup requires absolute config and appa paths.");
  if (!(await lstat(config)).isFile())
    throw Error("Setup config must be a regular file, not a symlink.");
  let target = process.env.APPA_BATTERY_TARGET ?? "single-user";
  if (!["single-user", "multi-user"].includes(target))
    throw Error("APPA_BATTERY_TARGET must be single-user or multi-user.");
  let store = process.env.APPA_OPENCODE_BATTERIES || join(dirname(config), "batteries"), catalog = await Promise.all((await readdir(store)).sort().filter((name) => !name.includes(".")).map(async (name) => {
    let manifest = Bun.TOML.parse(await readFile(join(store, name, "appa-package.toml"), "utf8"));
    return {
      name,
      description: manifest.description,
      hosts: manifest.battery.hosts,
      namespaces: manifest.battery.namespaces ?? [],
      path: `batteries/${name}/appa.toml`,
      target_policy: manifest.battery.targets?.[target]?.policy
    };
  })).then((items) => items.filter((item) => item.hosts.includes("opencode"))), publishing = !1;
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
    let text = await readFile(path, "utf8");
    found.set(path, text);
    let includes = Bun.TOML.parse(text).include ?? [];
    for (let include of includes)
      await documents(resolve(dirname(path), include), found);
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
        let policy = await readFile(config, "utf8");
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
        return JSON.stringify({
          ...battery,
          deployment_target: target,
          files: Object.fromEntries(await Promise.all(["appa-package.toml", "appa.toml", "README.md", ...battery.target_policy ? [battery.target_policy] : []].map(async (name) => [name, await readFile(join(store, battery.name, name), "utf8")]))),
          contracts: await readFile(assets ? join(assets, "contracts.md") : join(checkout, "website/content/docs/contracts.md"), "utf8")
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
        let candidate = join(dirname(config), `.appa-setup-${crypto.randomUUID()}.toml`);
        try {
          let previous = await readFile(config, "utf8");
          if (digest(previous) !== input.expected_digest)
            throw Error("Policy changed; inspect and propose again.");
          if (previous === input.policy)
            return "Policy unchanged. No new session is needed.";
          await writeFile(candidate, input.policy, { mode: 384, flag: "wx" });
          let description = await describe(candidate, tools.map((tool) => tool.canonical));
          if (digest(await readFile(config, "utf8")) !== input.expected_digest)
            throw Error("Policy changed during validation; inspect and propose again.");
          return await rename(candidate, config), JSON.stringify({
            state: "saved",
            description,
            inventory: inventory2,
            next: "Close setup. Restart the protected runtime and start a fresh OpenCode session. Existing sessions retain their opening policy."
          });
        } finally {
          publishing = !1, await rm(candidate, { force: !0 });
        }
      }
    }
  };
}

// integrations/opencode/appencode.ts
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
    let base = process.env.APPA_OPENCODE_RUNTIME_URL || ctx.options?.runtimeURL || "http://127.0.0.1:8789", setupConfig = process.env.APPA_OPENCODE_SETUP_CONFIG, setup = setupConfig ? yield* Effect4.tryPromise(() => configuration(setupConfig, process.env.APPA_OPENCODE_SETUP_BINARY || "")).pipe(Effect4.orDie) : void 0, setupGuide = setup ? yield* Effect4.tryPromise(guide).pipe(Effect4.orDie) : void 0, pending = /* @__PURE__ */ new Map, children = /* @__PURE__ */ new Map, initialPrompts = /* @__PURE__ */ new Set, launches = /* @__PURE__ */ new Map;
    async function post(event, actor, fields = {}) {
      if (event === "tool_call" && actor.child && !children.has(actor.child))
        throw Error("[appencode] Child has no live foreground delegation.");
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
      }), reply = await response2.json();
      if (!reply || reply.protocol !== 1 || typeof reply.decision !== "string")
        throw Error("[appencode] Invalid runtime reply. Check --adapter opencode and the runtime logs.");
      if (reply.decision === "refuse")
        throw Error(`[appencode] ${text(reply, "detail")}`);
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
          throw Error("[appencode] Child has no live foreground delegation. Resume and alternate launches are refused.");
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
        throw Error("[appencode] Child has no live foreground delegation.");
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
      if (editor.remove("subagent"), setup)
        editor.add({
          name: "appa_setup",
          options: { codemode: !1 },
          description: "Inspect AppenCode policy and live tools, read a catalog battery, or publish a complete approved policy. Publish only after a later operator message approves your proposal. Configuration access only; no coding or shell execution.",
          input: Schema29.Struct({
            operation: Schema29.Literals(["inspect", "battery", "publish"]),
            battery: Schema29.optionalKey(Schema29.String),
            policy: Schema29.optionalKey(Schema29.String),
            expected_digest: Schema29.optionalKey(Schema29.String)
          }),
          output: Schema29.String,
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
            return { output, content: output };
          }).pipe(Effect4.catch((error) => Effect4.fail(error instanceof exports_tool.Error ? error : failure(error))))
        });
      editor.add({
        name: "appa_subagent",
        options: { codemode: !1 },
        description: "Delegate to a fresh foreground subagent through APPA. Declare its return using the offered remedy, then retry. APPA authorizes delegation; native subagent permissions do not apply. No background, resume, or nested delegation.",
        input: Schema29.Struct({
          agent: Schema29.String,
          description: Schema29.String,
          prompt: Schema29.String,
          model: Schema29.optionalKey(Schema29.String),
          sessionID: Schema29.optionalKey(Schema29.String),
          background: Schema29.optionalKey(Schema29.Boolean)
        }),
        output: Schema29.String,
        execute: (input, call) => Effect4.gen(function* () {
          if (children.has(call.sessionID) || input.sessionID !== void 0 || input.background === !0)
            throw Error("[appencode] Only fresh, one-level foreground delegation is supported.");
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
          let proposal = { tool: "appa_subagent", arguments: structuredClone(input), call_id: call.id }, tools = yield* Effect4.tryPromise({ try: inventory2, catch: failure }), runtime = (event, fields = {}) => Effect4.tryPromise({ try: () => post(event, { root: call.sessionID }, fields), catch: failure }), releaseFields = { ...proposal, cwd: ctx.location.directory, inventory: { tools } }, release = "unattempted", settled = !1, childID, cleanup = Effect4.gen(function* () {
            if (childID)
              children.delete(childID), initialPrompts.delete(childID), yield* ctx.session.interrupt({ sessionID: childID, resume: !1 }).pipe(Effect4.catchCause(() => Effect4.void)), yield* ctx.session.wait({ sessionID: childID }).pipe(Effect4.catchCause(() => Effect4.void));
            if (settled)
              return;
            if (release === "uncertain")
              release = (yield* runtime("tool_call", releaseFields)).decision === "allow_call" ? "approved" : "unattempted";
            if (release === "approved") {
              if (childID)
                yield* runtime("child_end", { child_id: childID }).pipe(Effect4.catchCause(() => Effect4.void));
              settled = (yield* runtime("spawn_result", { ...proposal, spawned_id: childID, outcome: { status: "indeterminate" } })).decision === "ack";
            }
          }).pipe(Effect4.catchCause(() => Effect4.void)), launch = launches.get(call.sessionID);
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
            initialPrompts.add(child.id), yield* ctx.session.prompt({ sessionID: child.id, id: child.promptID, text: child.prompt }), yield* ctx.session.wait({ sessionID: child.id });
            let answer = [...yield* Effect4.tryPromise({ try: () => childHistory(child.id), catch: failure })].reverse().find((message) => message.type === "assistant");
            if (!answer || answer.type !== "assistant" || answer.error || !answer.time.completed || answer.finish !== "stop")
              throw Error("[appencode] Child did not complete a final answer; its return is withheld.");
            let value = answer.content.filter((part) => part.type === "text").map((part) => part.text).join(`
`), returned = yield* runtime("child_end", { child_id: childID, value });
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
      });
      for (let tool of editor.list())
        editor.update(tool.id, (item) => {
          let { pinned: _pinned, ...options } = item.options ?? {};
          item.options = { ...options, codemode: !1 };
        });
    }), yield* ctx.permission.hook("evaluate", (event) => Effect4.sync(() => {
      if (event.action === "execute" || event.action === "subagent")
        event.effect = "deny", event.message = "Use APPA foreground delegation, not Code Mode or native subagent returns.";
    })), yield* ctx.session.hook("prompt", (event) => Effect4.tryPromise({
      try: async () => {
        let session = await Effect4.runPromise(ctx.session.get({ sessionID: event.sessionID }));
        if (session.fork)
          throw Error("[appencode] Forked sessions are not supported.");
        if (!session.parentID)
          return;
        let child = children.get(event.sessionID);
        if (!child || !initialPrompts.delete(event.sessionID) || event.messageID !== child.promptID || event.prompt.text !== child.prompt || event.prompt.files?.length || event.prompt.agents?.length || event.prompt.skills?.length)
          throw Error("[appencode] Child prompts require an exact live foreground delegation; steering and alternate launches are refused.");
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
        let { context } = await open(event.sessionID);
        if (delete event.tools.execute, delete event.tools.subagent, children.has(event.sessionID))
          delete event.tools.appa_subagent;
        if (setup) {
          for (let name of Object.keys(event.tools))
            if (name !== "appa_setup")
              delete event.tools[name];
        }
        event.system.push({ type: "text", text: setupGuide || context || "AppenCode checks direct tool flows through APPA. Use the offered remedy tool, then retry. Do not bypass refused flows." });
      },
      catch: failure
    }).pipe(Effect4.orDie)), yield* ctx.tool.hook("execute.before", (event) => Effect4.tryPromise({
      try: async () => {
        if (event.tool === "appa_subagent") {
          if (await open(event.sessionID), children.has(event.sessionID))
            throw Error("[appencode] Nested delegation is not supported.");
          return;
        }
        if (event.tool === "execute" || event.tool === "subagent")
          throw Error("[appencode] Code Mode and native subagents are refused. Use direct tools or appa_subagent.");
        if (event.tool === "shell" && typeof event.input === "object" && event.input !== null && "background" in event.input && event.input.background === !0)
          throw Error("[appencode] Background shell results use an unchecked notification path. Use a foreground call.");
        let { actor } = await open(event.sessionID), tools = await inventory2(), tool = tools.find((tool2) => tool2.name === event.tool);
        if (!tool)
          throw Error("[appencode] Tool is absent from the current registry.");
        if (event.tool === "read")
          event.input = await checkedRead(event.input, ctx.location.directory);
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
  let path = await realpath2(resolve2(directory, input.path)).catch(() => {
    throw Error("[appencode] Cannot resolve the read path. Check that it exists and is accessible; alternate filenames are refused in v1.");
  }), root = await realpath2(directory), within = relative(root, path);
  if (path === root || !isAbsolute2(within) && within !== ".." && !within.startsWith(`..${sep}`)) {
    let current = (await stat(path)).isDirectory() ? path : dirname2(path);
    while (current !== root && current !== dirname2(current)) {
      let instruction = join2(current, "AGENTS.md");
      if (instruction !== path)
        try {
          throw await lstat2(instruction), Error("[appencode] This read would inject unchecked nearby AGENTS.md instructions. V1 refuses it.");
        } catch (error) {
          if (!error || typeof error !== "object" || !("code" in error) || error.code !== "ENOENT")
            throw error;
        }
      current = dirname2(current);
    }
  }
  return { ...input, path };
}
export {
  appencode_default as default
};
