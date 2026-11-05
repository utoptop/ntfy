import * as React from "react";
import { useEffect, useRef, useState } from "react";
import { Typography, Button, Box, CircularProgress } from "@mui/material";
import CheckCircleOutlineIcon from "@mui/icons-material/CheckCircleOutlineOutlined";
import ErrorOutlineIcon from "@mui/icons-material/ErrorOutlineOutlined";
import { useParams, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import accountApi from "../app/AccountApi";
import AvatarBox from "./AvatarBox";
import routes from "./routes";

// Verification states for the email-verify landing page
const STATUS_VERIFYING = "verifying";
const STATUS_SUCCESS = "success";
const STATUS_ERROR = "error";

// EmailVerify is the magic-link landing page for email verification. It performs the verification
// via a POST (the GET that loads this page has no side effects, so link prefetchers / scanners
// cannot consume the single-use token). The raw token is stripped from the URL on load to keep
// it out of browser history and Referer headers.
const EmailVerify = () => {
  const { t } = useTranslation();
  const { token } = useParams();
  const navigate = useNavigate();
  const [status, setStatus] = useState(STATUS_VERIFYING);
  const ran = useRef(false);

  useEffect(() => {
    if (ran.current) {
      return; // Guard against double-invoke (e.g. React StrictMode) consuming the token twice
    }
    ran.current = true;
    // Strip the token from the URL immediately (keep it out of history / Referer)
    window.history.replaceState(null, "", routes.account);
    (async () => {
      try {
        await accountApi.verifyEmailToken(token);
        setStatus(STATUS_SUCCESS);
      } catch (e) {
        console.log(`[EmailVerify] Verification failed`, e);
        setStatus(STATUS_ERROR);
      }
    })();
  }, [token]);

  return (
    <AvatarBox>
      {status === STATUS_VERIFYING && (
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <CircularProgress size={24} />
          <Typography sx={{ typography: "h6" }}>{t("email_verify_progress_title")}</Typography>
        </Box>
      )}
      {status === STATUS_SUCCESS && (
        <>
          <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
            <CheckCircleOutlineIcon color="success" sx={{ fontSize: 28 }} />
            <Typography sx={{ typography: "h6" }}>{t("email_verify_success_title")}</Typography>
          </Box>
          <Typography sx={{ mt: 1, textAlign: "center" }}>{t("email_verify_success_description")}</Typography>
          <Button onClick={() => navigate(routes.account)} variant="contained" sx={{ mt: 2 }}>
            {t("email_verify_button_account")}
          </Button>
        </>
      )}
      {status === STATUS_ERROR && (
        <>
          <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
            <ErrorOutlineIcon color="error" sx={{ fontSize: 28 }} />
            <Typography sx={{ typography: "h6" }}>{t("email_verify_error_title")}</Typography>
          </Box>
          <Typography sx={{ mt: 1, textAlign: "center" }}>{t("email_verify_error_description")}</Typography>
          <Button onClick={() => navigate(routes.account)} variant="contained" sx={{ mt: 2 }}>
            {t("email_verify_button_account")}
          </Button>
        </>
      )}
    </AvatarBox>
  );
};

export default EmailVerify;
