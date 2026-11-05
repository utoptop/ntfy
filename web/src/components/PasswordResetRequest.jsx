import * as React from "react";
import { useState } from "react";
import { TextField, Button, Box, Typography } from "@mui/material";
import CheckCircleOutlineIcon from "@mui/icons-material/CheckCircleOutlineOutlined";
import { NavLink } from "react-router-dom";
import { useTranslation } from "react-i18next";
import accountApi from "../app/AccountApi";
import AvatarBox from "./AvatarBox";
import routes from "./routes";

// PasswordResetRequest is the standalone "request a password reset" page, reached from the login page.
// It collects a username/email and asks the server to email a reset link. The response is uniform,
// so the page always shows the same confirmation. Completing the reset happens on the separate
// PasswordReset landing page that the emailed link points to.
const PasswordResetRequest = () => {
  const { t } = useTranslation();
  const [identifier, setIdentifier] = useState("");
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);

  const handleSubmit = async (event) => {
    event.preventDefault();
    try {
      setSending(true);
      await accountApi.requestPasswordReset(identifier);
    } catch (e) {
      console.log(`[PasswordResetRequest] Request failed`, e);
    } finally {
      setSending(false);
      setSent(true); // Uniform outcome regardless of success/failure (enumeration-safe)
    }
  };

  if (!config.enable_reset_password) {
    return (
      <AvatarBox>
        <Typography sx={{ typography: "h6" }}>{t("reset_password_disabled")}</Typography>
        <Typography sx={{ mt: 2 }}>
          <NavLink to={routes.login} variant="body1">
            {t("reset_password_back_to_login")}
          </NavLink>
        </Typography>
      </AvatarBox>
    );
  }

  if (sent) {
    return (
      <AvatarBox>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <CheckCircleOutlineIcon color="success" sx={{ fontSize: 28 }} />
          <Typography sx={{ typography: "h6" }}>{t("reset_password_sent_title")}</Typography>
        </Box>
        <Typography sx={{ mt: 1, textAlign: "center" }}>{t("reset_password_sent_description")}</Typography>
        <Typography sx={{ mt: 2, mb: 4 }}>
          <NavLink to={routes.login} variant="body1">
            {t("reset_password_back_to_login")}
          </NavLink>
        </Typography>
      </AvatarBox>
    );
  }

  return (
    <AvatarBox>
      <Typography sx={{ typography: "h6" }}>{t("reset_password_request_title")}</Typography>
      <Box component="form" onSubmit={handleSubmit} noValidate sx={{ mt: 1 }}>
        <Typography sx={{ mt: 1 }}>{t("reset_password_request_description")}</Typography>
        <Typography sx={{ mt: 1, mb: 1.5, fontWeight: "bold" }}>{t("reset_password_request_primary_required")}</Typography>
        <TextField
          margin="dense"
          required
          fullWidth
          id="identifier"
          label={t("reset_password_request_identifier_label")}
          name="identifier"
          value={identifier}
          onChange={(ev) => setIdentifier(ev.target.value.trim())}
          autoFocus
        />
        <Button type="submit" fullWidth variant="contained" disabled={sending || identifier === ""} sx={{ mt: 2, mb: 2 }}>
          {t("reset_password_request_button_submit")}
        </Button>
      </Box>
      {config.enable_login && (
        <Typography sx={{ mb: 4 }}>
          <NavLink to={routes.login} variant="body1">
            {t("reset_password_back_to_login")}
          </NavLink>
        </Typography>
      )}
    </AvatarBox>
  );
};

export default PasswordResetRequest;
