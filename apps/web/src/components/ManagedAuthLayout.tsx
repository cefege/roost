// Managed public auth pages need one accessible branded shell without duplicating structure.
// Login, signup, activation, and recovery routes compose their forms through this layout.
// BrandMark and managed-auth styles provide presentation while each caller owns behavior.

import type { JSX } from "solid-js";
import { BrandMark } from "./BrandMark.tsx";
import { Surface } from "./Settings/md/Surface.tsx";
import "./managed-auth.css";

export function ManagedAuthLayout(props: {
  testId: string;
  title: string;
  description: string;
  children?: JSX.Element;
}) {
  const titleId = `${props.testId}-title`;
  return (
    <main class="managed-auth-page" data-testid={props.testId}>
      <Surface
        as="section"
        class="managed-auth-sheet"
        level={1}
        elevation={3}
        radius="xl"
        border
        data-testid={`${props.testId}-sheet`}
        aria-labelledby={titleId}
      >
        <header class="managed-auth-header">
          <div class="managed-auth-brand" aria-label="Roost">
            <BrandMark />
            <span>Roost</span>
          </div>
          <h1 class="managed-auth-title" id={titleId}>{props.title}</h1>
          <p class="managed-auth-description">{props.description}</p>
        </header>
        {props.children}
        <nav class="managed-auth-legal" aria-label="Legal and support">
          <a class="managed-auth-link" href="https://roosttt.com/privacy/" rel="noopener">Privacy</a>
          <a class="managed-auth-link" href="https://roosttt.com/terms/" rel="noopener">Terms</a>
          <a class="managed-auth-link" href="mailto:support@roosttt.com">Support</a>
        </nav>
      </Surface>
    </main>
  );
}
