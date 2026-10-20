/* @refresh reload */
import { render } from "solid-js/web";
import { Router, Route } from "@solidjs/router";
import "@cell/ui/src/theme.css";
import { Layout } from "./App";
import { Library } from "./pages/Library";
import { GamePage } from "./pages/Game";
import { Servers } from "./pages/Servers";
import { ServerPage } from "./pages/Server";
import { Upload } from "./pages/Upload";
import { Profile } from "./pages/Profile";
import { Members } from "./pages/Members";
import { Peers } from "./pages/Peers";

render(
  () => (
    <Router root={Layout}>
      <Route path="/" component={Library} />
      <Route path="/games/:id" component={GamePage} />
      <Route path="/servers" component={Servers} />
      <Route path="/servers/:id" component={ServerPage} />
      <Route path="/upload" component={Upload} />
      <Route path="/profile" component={Profile} />
      <Route path="/members" component={Members} />
      <Route path="/peers" component={Peers} />
    </Router>
  ),
  document.getElementById("root")!,
);
