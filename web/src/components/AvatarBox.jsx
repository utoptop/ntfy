import * as React from "react";
import { Avatar, Box, styled } from "@mui/material";
import { useNavigate } from "react-router-dom";
import logo from "../img/ntfy-filled.svg";
import routes from "./routes";
import { fadeNavigate } from "../app/transition";

const AvatarBoxContainer = styled(Box)`
  display: flex;
  flex-grow: 1;
  justify-content: center;
  flex-direction: column;
  align-content: center;
  align-items: center;
  height: 100dvh;
  max-width: min(400px, 90dvw);
  margin: auto;
`;
const AvatarBox = (props) => {
  const navigate = useNavigate();
  const avatar = <Avatar sx={{ m: 2, width: 64, height: 64, borderRadius: 3 }} src={logo} variant="rounded" />;
  // Fade back to the app instead of a hard cut. Let modifier-clicks (open in new tab, etc.) through.
  const handleLogoClick = (ev) => {
    if (ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.altKey) {
      return;
    }
    ev.preventDefault();
    fadeNavigate(navigate, routes.app);
  };
  return (
    <AvatarBoxContainer>
      {/* The logo links back to the app, unless login is forced (no app to go back to without signing in) */}
      {config.require_login ? (
        avatar
      ) : (
        <Box component="a" href={routes.app} onClick={handleLogoClick} sx={{ cursor: "pointer", lineHeight: 0 }}>
          {avatar}
        </Box>
      )}
      {props.children}
    </AvatarBoxContainer>
  );
};

export default AvatarBox;
