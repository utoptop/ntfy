import * as React from "react";
import { useEffect, useRef, useState } from "react";
import { Typography, TextField, Button, Box } from "@mui/material";
import WarningAmberIcon from "@mui/icons-material/WarningAmber";
import CheckCircleOutlineIcon from "@mui/icons-material/CheckCircleOutlineOutlined";
import { useParams, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import accountApi from "../app/AccountApi";
import AvatarBox from "./AvatarBox";
import routes from "./routes";

// PasswordReset is the magic-link landing page for setting a new password. There is no
// pre-validation: the form renders directly and an invalid/expired token surfaces as an error on
// submit. The raw token is stripped from the URL on load (kept out of history / Referer).
const PasswordReset = () => {
  const { t } = useTranslation();
  const { token: tokenParam } = useParams();
  const navigate = useNavigate();
  const token = useRef(tokenParam);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState("");
  const [sending, setSending] = useState(false);
  const [done, setDone] = useState(false);

  useEffect(() => {
    // Strip the token from the URL bar immediately (keep it out of history / Referer)
    window.history.replaceState(null, "", routes.login);
  }, []);

  const handleSubmit = async (event) => {
    event.preventDefault();
    try {
      setSending(true);
      setError("");
      await accountApi.resetPassword(token.current, password);
      setDone(true);
    } catch (e) {
      console.log(`[PasswordReset] Reset failed`, e);
      setError(t("reset_password_form_error_invalid"));
    } finally {
      setSending(false);
    }
  };

  if (done) {
    return (
      <AvatarBox>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <CheckCircleOutlineIcon color="success" sx={{ fontSize: 28 }} />
          <Typography sx={{ typography: "h6" }}>{t("reset_password_success_title")}</Typography>
        </Box>
        <Typography sx={{ mt: 1, textAlign: "center" }}>{t("reset_password_success_description")}</Typography>
        <Button onClick={() => navigate(routes.login)} variant="contained" sx={{ mt: 2 }}>
          {t("login_form_button_submit")}
        </Button>
      </AvatarBox>
    );
  }

  return (
    <AvatarBox>
      <Typography sx={{ typography: "h6" }}>{t("reset_password_title")}</Typography>
      <Box component="form" onSubmit={handleSubmit} noValidate sx={{ mt: 1 }}>
        <TextField
          margin="dense"
          required
          fullWidth
          name="password"
          label={t("reset_password_form_password")}
          type="password"
          id="password"
          value={password}
          onChange={(ev) => setPassword(ev.target.value)}
          autoComplete="new-password"
          autoFocus
        />
        <TextField
          margin="dense"
          required
          fullWidth
          name="confirm"
          label={t("reset_password_form_confirm")}
          type="password"
          id="confirm"
          value={confirm}
          onChange={(ev) => setConfirm(ev.target.value)}
          autoComplete="new-password"
        />
        <Button
          type="submit"
          fullWidth
          variant="contained"
          disabled={sending || password === "" || confirm === "" || password !== confirm}
          sx={{ mt: 2, mb: 2 }}
        >
          {t("reset_password_form_button_submit")}
        </Button>
        {error && (
          <Box sx={{ mb: 1, display: "flex", flexGrow: 1, justifyContent: "center" }}>
            <WarningAmberIcon color="error" sx={{ mr: 1 }} />
            <Typography sx={{ color: "error.main" }}>{error}</Typography>
          </Box>
        )}
      </Box>
    </AvatarBox>
  );
};

export default PasswordReset;
