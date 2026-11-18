import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { Center } from "@astryxdesign/core/Center";
import { FormLayout } from "@astryxdesign/core/FormLayout";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { Layout, LayoutContent, LayoutHeader } from "@astryxdesign/core/Layout";
import { Spinner } from "@astryxdesign/core/Spinner";
import { Switch } from "@astryxdesign/core/Switch";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { VStack } from "@astryxdesign/core/VStack";
import { ArrowLeft } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useBlocker, useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { EntityRecord, JsonObject } from "@/api/types";
import { Lookup } from "@/components/lookup";
import { t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { entity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useSession } from "@/session";

interface Form {
  code: string;
  name: string;
  active: boolean;
  payment_terms_id: string | null;
}

const EMPTY: Form = { code: "", name: "", active: true, payment_terms_id: null };

function formOf(record: EntityRecord): Form {
  return {
    code: String(record.code ?? ""),
    name: String(record.name ?? ""),
    active: record.active !== false,
    payment_terms_id: typeof record.payment_terms_id === "string" ? record.payment_terms_id : null,
  };
}

/** A simple master record: the customer, with its fields from the registry. */
export function CustomerPage() {
  const { company: code = "", id } = useParams();
  const { meta } = useSession();
  const navigate = useNavigate();
  const customer = entity(meta, "customer");
  const field = (name: string) => customer?.fields.find((f) => f.name === name);
  const label = (name: string) => field(name)?.label ?? name;

  const [record, setRecord] = useState<EntityRecord | null>(null);
  const [form, setForm] = useState<Form | null>(id ? null : EMPTY);
  const [saved, setSaved] = useState<Form>(EMPTY);
  const [error, setError] = useState<{ field: string | null; message: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const idempotency = useRef(crypto.randomUUID());

  useEffect(() => {
    if (!id) return;
    const controller = new AbortController();
    api.run("entity.get", { entity: "customer", id }, { signal: controller.signal }).then(
      (r) => {
        setRecord(r);
        setForm(formOf(r));
        setSaved(formOf(r));
      },
      (cause: unknown) => {
        if (!controller.signal.aborted) setError({ field: null, message: cause instanceof Error ? cause.message : String(cause) });
      },
    );
    return () => controller.abort();
  }, [id]);

  const dirty = form !== null && JSON.stringify(form) !== JSON.stringify(saved);
  const blocker = useBlocker(({ currentLocation, nextLocation }) => dirty && !saving && currentLocation.pathname !== nextLocation.pathname);
  useEffect(() => {
    if (blocker.state !== "blocked") return;
    if (window.confirm(t("doc.leave"))) blocker.proceed();
    else blocker.reset();
  }, [blocker]);

  const save = useCallback(async () => {
    if (!form || saving) return;
    setSaving(true);
    setError(null);
    try {
      const data: JsonObject = { ...form };
      const options = { idempotencyKey: idempotency.current };
      const next =
        id && record
          ? await api.run("entity.update", { entity: "customer", id, data, version: record._version ?? null }, options)
          : await api.run("entity.create", { entity: "customer", data }, options);
      idempotency.current = crypto.randomUUID();
      setRecord(next);
      setForm(formOf(next));
      setSaved(formOf(next));
      if (!id) navigate(paths.customer(code, next.id), { replace: true });
    } catch (cause) {
      setError(
        cause instanceof ApiError
          ? { field: cause.field, message: cause.message }
          : { field: null, message: String(cause) },
      );
    } finally {
      setSaving(false);
    }
  }, [form, saving, id, record, navigate, code]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [save]);

  if (!form) {
    return error ? (
      <VStack padding={4}>
        <Banner status="error" title={error.message} />
      </VStack>
    ) : (
      <Center height="100%">
        <Spinner label={t("common.loading")} />
      </Center>
    );
  }

  const statusFor = (name: string) => (error?.field === name ? { type: "error" as const, message: error.message } : undefined);
  const set = <K extends keyof Form>(key: K, value: Form[K]) => setForm((f) => (f ? { ...f, [key]: value } : f));

  return (
    <Layout
      height="fill"
      header={<LayoutHeader hasDivider padding={3}>
        <HStack hAlign="between" vAlign="center" gap={3} wrap="wrap">
          <HStack gap={3} vAlign="center">
            <IconButton label={t("common.back")} icon={<Icon icon={ArrowLeft} />} variant="ghost" size="sm" onClick={() => navigate(-1)} />
            <VStack gap={0.5}>
              <Text type="supporting">{customer?.label ?? "Customer"}</Text>
              <Heading level={1}>{record ? `${saved.name}` : t("customer.new")}</Heading>
            </VStack>
          </HStack>
          <HStack gap={2} vAlign="center">
            {dirty ? <Badge variant="warning" label={t("doc.unsaved")} /> : null}
            <Button label={t("doc.save")} variant="primary" isLoading={saving} onClick={() => void save()} tooltip="Ctrl+S" />
          </HStack>
        </HStack>
      </LayoutHeader>}
      content={<LayoutContent padding={4}>
        <VStack gap={4} maxWidth={720}>
          {error && !error.field ? <Banner status="error" title={t("customer.saveFailed")} description={error.message} /> : null}
          <Card padding={4}>
            <FormLayout>
              <TextInput
                label={label("code")}
                value={form.code}
                onChange={(v) => set("code", v)}
                isRequired={field("code")?.required}
                status={statusFor("code")}
                hasAutoFocus={!id}
              />
              <TextInput
                label={label("name")}
                value={form.name}
                onChange={(v) => set("name", v)}
                isRequired={field("name")?.required}
                status={statusFor("name")}
              />
              <Lookup
                label={label("payment_terms_id")}
                entity="payment_terms"
                value={form.payment_terms_id}
                onChange={(v) => set("payment_terms_id", v)}
                error={error?.field === "payment_terms_id" ? error.message : null}
              />
              <Switch
                label={label("active")}
                value={form.active}
                onChange={(v) => set("active", v)}
              />
            </FormLayout>
          </Card>
          {record && typeof record.created_at === "string" ? (
            <Text type="supporting">
              {label("created_at")}: {formatTimestamp(record.created_at)}
            </Text>
          ) : null}
        </VStack>
      </LayoutContent>}
    />
  );
}
