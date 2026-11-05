import * as React from "react";
import { Link, Button } from "@mui/material";
import { Trans, withTranslation } from "react-i18next";
import { copyToClipboard } from "../app/utils";

class ErrorBoundaryImpl extends React.Component {
  constructor(props) {
    super(props);
    this.state = {
      error: false,
      stack: null,
      unsupportedIndexedDB: false,
    };
  }

  componentDidCatch(error, info) {
    console.error("[ErrorBoundary] Error caught", error, info);

    // Special case for unsupported IndexedDB in Private Browsing mode (Firefox, Safari), see
    // - https://github.com/dexie/Dexie.js/issues/312
    // - https://bugzilla.mozilla.org/show_bug.cgi?id=781982
    const isUnsupportedIndexedDB =
      error?.name === "InvalidStateError" || (error?.name === "DatabaseClosedError" && error?.message?.indexOf("InvalidStateError") !== -1);

    if (isUnsupportedIndexedDB) {
      this.handleUnsupportedIndexedDB();
    } else {
      this.handleError(error, info);
    }
  }

  handleError(error, info) {
    const componentStack = info.componentStack
      .trim()
      .split("\n")
      .map((line) => `  at ${line}`)
      .join("\n");
    const parts = [error.toString()];
    if (error.stack) parts.push(error.stack);
    parts.push(componentStack);
    this.setState({
      error: true,
      stack: parts.join("\n"),
    });
  }

  handleUnsupportedIndexedDB() {
    this.setState({
      error: true,
      unsupportedIndexedDB: true,
    });
  }

  copyStack() {
    copyToClipboard(`${this.state.stack}\n`);
  }

  renderUnsupportedIndexedDB() {
    const { t } = this.props;
    return (
      <div style={{ margin: "20px" }}>
        <h2>{t("error_boundary_unsupported_indexeddb_title")} 😮</h2>
        <p style={{ maxWidth: "600px" }}>
          <Trans
            i18nKey="error_boundary_unsupported_indexeddb_description"
            components={{
              githubLink: <Link href="https://github.com/binwiederhier/ntfy/issues/208" />,
              discordLink: <Link href="https://discord.gg/cT7ECsZj9w" />,
              matrixLink: <Link href="https://matrix.to/#/#ntfy:matrix.org" />,
            }}
          />
        </p>
      </div>
    );
  }

  renderError() {
    const { t } = this.props;
    return (
      <div style={{ margin: "20px" }}>
        <h2>{t("error_boundary_title")} 😮</h2>
        <p>
          <Trans
            i18nKey="error_boundary_description"
            components={{
              githubLink: <Link href="https://github.com/binwiederhier/ntfy/issues" />,
              discordLink: <Link href="https://discord.gg/cT7ECsZj9w" />,
              matrixLink: <Link href="https://matrix.to/#/#ntfy:matrix.org" />,
            }}
          />
        </p>
        <div style={{ display: "flex", gap: 5 }}>
          <Button variant="outlined" onClick={() => this.copyStack()}>
            {t("error_boundary_button_copy_stack_trace")}
          </Button>

          <Button variant="outlined" onClick={() => window.location.reload()}>
            {t("error_boundary_button_reload_ntfy")}
          </Button>
        </div>
        <h3>{t("error_boundary_stack_trace")}</h3>
        <pre>{this.state.stack}</pre>
      </div>
    );
  }

  render() {
    if (this.state.error) {
      if (this.state.unsupportedIndexedDB) {
        return this.renderUnsupportedIndexedDB();
      }
      return this.renderError();
    }
    return this.props.children;
  }
}

const ErrorBoundary = withTranslation()(ErrorBoundaryImpl); // Adds props.t
export default ErrorBoundary;
