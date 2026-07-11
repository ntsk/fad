# fad

Firebase App Distribution から APK / AAB をダウンロードして Android 端末にインストールする CLI ツール。

## 必要なもの

- `adb`（PATH 上にあること）
- `bundletool`（PATH 上にあること。AAB のインストール時に使用）
- 対象の Firebase プロジェクトにアクセスできる Google アカウント

## インストール

```
cargo install --path .
```

## 設定

`fad login` 後にアクセス可能な Firebase プロジェクトと Android アプリの一覧から対話的に選択でき、選択結果は `~/.config/fad/config.toml` に保存されます。設定がない状態で `fad install` を実行した場合も同じ選択が実行されます。

手動で設定する場合は `~/.config/fad/config.toml` を作成します。

```toml
app_id = "1:1234567890:android:0a1b2c3d4e5f"
```

`app_id` は Firebase コンソールのプロジェクト設定 > マイアプリで確認できます。プロジェクト番号は `app_id` から自動的に導出されます。

認証にはデフォルトで Firebase CLI と同じ公開 OAuth クライアントを使用します。独自の OAuth クライアント（デスクトップアプリ種別）を使う場合は次を追加してください。

```toml
[oauth]
client_id = "..."
client_secret = "..."
```

## 使い方

```
fad login             # ブラウザで Google アカウントにログインし、対象アプリを選択
fad install --list    # インストール可能なリリースの一覧を表示
fad install <ID>      # リリースをダウンロードしてインストール
```

対象アプリを切り替えたいときは `fad login` を再実行するか、`config.toml` の `app_id` を書き換えてください。

## 動作の詳細

- `fad login` はブラウザで OpenID Connect (OAuth 2.0) 認証を行い、トークンを `~/.config/fad/credentials.json` に保存します
- APK のリリースはそのまま `adb install -r` でインストールします
- AAB のリリースは `bundletool build-apks --mode=universal` で universal APK に変換してからインストールします（署名にはデフォルトの debug keystore `~/.android/debug.keystore` が使われます）
