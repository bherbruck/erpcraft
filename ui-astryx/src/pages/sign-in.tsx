import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { Center } from "@astryxdesign/core/Center";
import { FormLayout } from "@astryxdesign/core/FormLayout";
import { Heading } from "@astryxdesign/core/Heading";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { VStack } from "@astryxdesign/core/VStack";
import { type FormEvent, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { ApiError, api } from "@/api/client";
import { t } from "@/i18n/t";

export function SignInPage() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event?: FormEvent) => {
    event?.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.login(email, password);
      const next = params.get("next");
      navigate(next?.startsWith("/") && !next.startsWith("//") ? next : "/", { replace: true });
    } catch (cause) {
      // The typed email and password stay; only the message changes.
      setError(cause instanceof ApiError ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Center height="100dvh" padding={4}>
      <Card width="100%" maxWidth={400} padding={6}>
        <form onSubmit={submit} noValidate>
          <VStack gap={5}>
            <VStack gap={1}>
              <Heading level={1}>{t("signIn.title")}</Heading>
              <Text type="supporting">{t("signIn.subtitle")}</Text>
            </VStack>
            {error ? <Banner status="error" title={t("signIn.failed")} description={error} /> : null}
            <FormLayout>
              <TextInput
                label={t("signIn.email")}
                type="email"
                value={email}
                onChange={setEmail}
                autoComplete="username"
                isRequired
                hasAutoFocus
              />
              <TextInput
                label={t("signIn.password")}
                type="password"
                value={password}
                onChange={setPassword}
                autoComplete="current-password"
                isRequired
                onEnter={() => void submit()}
              />
            </FormLayout>
            <Button label={t("signIn.submit")} type="submit" variant="primary" isLoading={busy} width="100%" />
          </VStack>
        </form>
      </Card>
    </Center>
  );
}
