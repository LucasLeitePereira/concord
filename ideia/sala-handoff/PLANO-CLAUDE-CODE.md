# Especificação — Sala: app desktop leve de voz e tela (P2P)

Você vai construir este projeto do zero. Leia tudo antes de começar, incluindo a pasta `design/` (seção 7). Delegue e paralelize como achar melhor, mas respeite as **restrições** e entregue o que está em **Critérios de aceite**.

---

## 1. Contexto e objetivo

Dois amigos (raramente três) precisam de um substituto para o Discord, cujo vídeo e compartilhamento de tela estão suspensos no Brasil. Requisito central: **o app não pode afetar o desempenho de jogos**.

O app tem dois modos:

- **Modo jogo:** só um núcleo mínimo roda (voz + atalhos). A interface é encerrada de verdade, não minimizada.
- **Modo normal:** interface completa com compartilhamento de tela em alta qualidade, mesa de sons e chat temporário.

O app tem **custo zero de operação**: não existe servidor. Os PCs se conectam diretamente pela rede do **Tailscale**, que os usuários já terão instalado. Cada PC enxerga o outro por um IP `100.x.y.z`.

## 2. Restrições (não negociáveis)

- **Plataforma:** Windows 10/11 x64. Não precisa suportar outros sistemas.
- **Sem servidor**, sem contas, sem login, sem telemetria, sem chamadas a serviços externos.
- **Sem Electron.** Sem overlay, sem injeção de DLL e sem hooks em processos de jogos.
- **Chat nunca é gravado em disco.** Ele só existe na memória e é apagado ao encerrar a chamada.
- **Núcleo em Rust estável.** Mínimo de dependências: justifique cada crate pesada.
- **Código e identificadores em inglês. Textos da interface em português do Brasil.**
- Nada de Assembly ou otimização prematura. O desempenho vem da arquitetura (seção 3) e do encoder de hardware.

## 3. Arquitetura

Workspace Cargo com:

```
sala/
├── crates/
│   ├── protocol/   # tipos compartilhados: mensagens de rede e de IPC
│   ├── core/       # binário `sala-core`: processo principal, sempre ligado
│   └── ui/         # binário `sala-ui`: app Tauri 2, só no modo normal
├── assets/         # ícones, sons de exemplo
├── scripts/        # medição de desempenho, build, empacotamento
└── README.md
```

### `sala-core` (processo principal)

- É o executável que o usuário abre (atalho e inicialização com o Windows apontam para ele).
- Fica na bandeja do sistema (ícone + menu: mutar, entrar/sair do modo jogo, abrir interface, sair).
- É dono de **toda a lógica**: rede, voz, mesa de sons, atalhos globais, detecção de jogo, captura e envio de tela, visualizador de tela, estado do chat.
- Inicia e encerra o processo `sala-ui`.
- Expõe um servidor IPC por **named pipe** (`\\.\pipe\sala-core-<usuário>`) com mensagens JSON delimitadas por linha.

### `sala-ui` (interface Tauri 2)

- **Sem lógica de negócio.** Só exibe o estado e envia comandos ao núcleo pelo IPC.
- Frontend leve: Svelte ou TypeScript puro, sem bibliotecas de UI pesadas.
- Se for aberto diretamente sem o núcleo rodando, ele inicia o núcleo e se conecta a ele.
- Se o núcleo mandar `Shutdown`, ele sai imediatamente.

### Visualizador de tela

- É uma janela nativa (`winit` + `wgpu`) aberta **pelo núcleo** só enquanto o usuário assiste. Ela é destruída ao parar.
- O vídeo **não** passa pelo WebView.

## 4. Rede e protocolo

- **Transporte:** QUIC com a crate `quinn`, porta UDP padrão `47800` (configurável).
- **TLS:** certificado autoassinado gerado na primeira execução e salvo no diretório de configuração. Use **pinning por fingerprint**: na primeira conexão com um amigo, guarde o fingerprint dele (confiança no primeiro uso). Depois disso, recuse conexões com fingerprint diferente e mostre um aviso.
- **Só aceitar conexões** de endereços que estão na lista de amigos da configuração.
- O papel de quem disca: ambos escutam. Quem iniciar a chamada disca. Se houver conexão simultânea dos dois lados, mantenha a do lado com o fingerprint lexicograficamente menor.
- **Stream bidirecional confiável (controle):** mensagens serializadas (`serde` + `bincode` ou JSON, à sua escolha) com prefixo de tamanho. No mínimo:
  - `Hello { name, app_version, protocol_version, capabilities }`. `capabilities` inclui os codecs de vídeo que a máquina consegue **codificar** e **decodificar** por hardware.
  - `Status { muted, deafened, game_mode, sharing }`
  - `Chat { id, text, sent_at }`
  - `ShareStart { codec, width, height, fps }` / `ShareStop`
  - `KeyframeRequest`
  - `Bye`
- **Datagramas QUIC** (não confiáveis) para mídia. Respeite o tamanho máximo de datagrama da conexão (cerca de 1200 bytes):
  - Voz: `{ stream: Voice, seq: u16, timestamp: u32, opus_payload }`
  - Áudio da tela: o mesmo formato, com `stream: ShareAudio`
  - Vídeo: `{ stream: Video, frame_id: u32, frag_idx: u16, frag_count: u16, keyframe: bool, timestamp, payload }`
- Envie um sinal de vida periódico e reconecte automaticamente com backoff se a conexão cair. A interface mostra "Reconectando…".

## 5. Funcionalidades

### 5.1 Voz

- Captura e reprodução com `cpal` (WASAPI). Dispositivos de entrada e saída selecionáveis na configuração e na interface.
- 48 kHz, mono no microfone. Opus em modo VOIP, quadros de **20 ms**, bitrate padrão **96 kbps** (configurável de 32 a 128), FEC ligado, DTX ligado.
- **Jitter buffer adaptativo**, de 40 a 120 ms. Para pacotes perdidos, use a ocultação de perda do próprio Opus (PLC).
- Ativação por voz (limiar configurável) **ou** push-to-talk (atalho global).
- Supressão de ruído opcional com `nnnoiseless`, **desligada por padrão**.
- Volume por pessoa, mudo e ensurdecer.
- Sem cancelamento de eco (presume-se que os usuários usam fone de ouvido). Documente isso no README.
- **Modo de teste de áudio** (loopback local) para a pessoa ouvir o próprio microfone.

### 5.2 Modo jogo

- **Entrar no modo jogo:**
  1. O núcleo envia `Shutdown` à interface e confirma que o processo terminou.
  2. Fecha o visualizador de tela, se estiver aberto.
  3. Para o compartilhamento de tela, a menos que o usuário tenha marcado "manter transmissão no modo jogo".
  4. Libera buffers que não são necessários.
- **Sair:** o núcleo reabre a interface.
- **Gatilhos:**
  - Atalho global (padrão `Ctrl+Shift+G`).
  - Detecção automática (ligada por padrão). Verifique a janela em primeiro plano a cada 2 segundos com a API do Windows. Considere "jogo" se o executável estiver na lista de jogos da configuração **ou** se a janela ocupar o monitor inteiro e não for um processo da lista de exclusão (explorer, navegadores, players de vídeo; a lista é configurável). Use histerese: entre após 2 leituras positivas seguidas e saia após 3 negativas seguidas.
- No modo jogo, mensagens de chat que chegarem geram uma **notificação nativa do Windows** (toast). Não use overlay.
- Ao entrar ou sair do modo jogo, envie `Status` ao amigo.

### 5.3 Mesa de sons

- Pasta de sons em `%APPDATA%\Sala\sons\`. Formatos: WAV, MP3, OGG e FLAC (decodificação com `symphonia` e reamostragem para 48 kHz com `rubato`).
- Cada som pode ter um atalho global e um volume próprios, definidos na configuração e editáveis na interface.
- O som é **mixado na voz enviada** e, opcionalmente, tocado localmente.
- **Decodifique sob demanda.** Não mantenha todos os sons na memória.
- Os atalhos funcionam também no modo jogo.

### 5.4 Chat temporário

- As mensagens trafegam pelo stream de controle.
- O estado fica num `Vec` em memória dentro do núcleo. A interface consulta o histórico da sessão pelo IPC.
- Ao encerrar a chamada ou fechar o núcleo, o histórico é descartado. **Nunca grave em disco**, nem em logs.
- Somente texto. Links aparecem clicáveis na interface.

### 5.5 Compartilhamento de tela

- **Captura:** Windows Graphics Capture com a crate `windows-capture`. Permita escolher entre um monitor e uma janela, com prévia simples da lista na interface.
- **Codificação por hardware** via FFmpeg (`ffmpeg-next` ou bindings equivalentes), escolhida pela capacidade negociada no `Hello`:
  1. AV1 por hardware (`av1_nvenc` / `av1_amf` / `av1_qsv`) se **o transmissor codificar e o receptor decodificar** AV1 por hardware.
  2. Senão, HEVC por hardware, nas mesmas condições.
  3. Senão, H.264 por hardware (`h264_nvenc` / `h264_amf` / `h264_qsv`).
  4. Sem encoder de hardware: use `libx264` em preset `ultrafast` com teto de 720p30 e avise o usuário na interface.
- **Parâmetros de baixa latência:** sem B-frames, GOP longo com keyframe sob `KeyframeRequest`, controle de taxa CBR ou VBR com VBV curto, presets e tune de baixa latência do encoder correspondente.
- Evite cópias desnecessárias de quadros entre GPU e CPU. Se a cópia for inevitável no MVP, documente isso e deixe um `TODO` para o caminho zero-copy com D3D11.
- **Presets:**

  | Preset | Resolução | FPS | Bitrate alvo |
  |---|---|---|---|
  | Leve | 720p | 30 | 3 Mbps |
  | Padrão | 1080p | 60 | 8 Mbps |
  | Alta | 1440p | 60 | 14 Mbps |
  | Máxima | resolução nativa | 60 | 20 Mbps |

  O usuário também pode definir um bitrate manual.
- **Bitrate adaptativo:** use as estatísticas do QUIC (perda e RTT). Se a perda passar de 2%, reduza o bitrate em 20%. Com a rede estável por 10 segundos, aumente em 5%, sem passar do alvo do preset.
- **Áudio da tela:** captura de loopback WASAPI (o som do sistema), em Opus estéreo a 128 kbps, sincronizado com o vídeo pelos timestamps. Pode ser desligado.
- **Receptor:**
  - Monta os quadros a partir dos fragmentos. Se um quadro ficar incompleto, descarte-o e peça um keyframe (no máximo uma vez a cada 500 ms).
  - Decodifica por hardware (D3D11VA) e exibe no visualizador `wgpu` com buffer mínimo.
  - O visualizador tem tela cheia (F11), uma sobreposição de estatísticas (F3: FPS, bitrate, perda, latência) e um controle de volume do áudio da tela.

## 6. Configuração

O arquivo fica em `%APPDATA%\Sala\config.toml`, é criado com valores padrão comentados na primeira execução, e pode ser editado pela interface.

```toml
name = "Seu nome"
port = 47800

[[friends]]
name = "Amigo"
address = "100.x.y.z"
# fingerprint é preenchido automaticamente na primeira conexão

[audio]
input_device = "default"
output_device = "default"
bitrate_kbps = 96
mode = "voice_activity"   # ou "push_to_talk"
vad_threshold = -45
noise_suppression = false

[hotkeys]
push_to_talk = "Mouse4"
toggle_mute = "Ctrl+Shift+M"
toggle_game_mode = "Ctrl+Shift+G"

[game_mode]
auto_detect = true
games = ["valorant.exe", "cs2.exe"]
exclude = ["explorer.exe", "chrome.exe", "msedge.exe", "firefox.exe", "vlc.exe", "mpv.exe"]
keep_sharing_in_game_mode = false

[share]
preset = "padrao"
share_audio = true

[[soundboard]]
file = "exemplo.ogg"
hotkey = "Ctrl+Numpad1"
volume = 0.8
```

Os logs (`tracing`) vão para `%APPDATA%\Sala\logs\`, com rotação diária, nível `info` por padrão e **nenhum conteúdo de chat**.

## 7. Interface (`sala-ui`)

**O design já está pronto e é obrigatório.** Antes de escrever qualquer código de interface, leia `design/DESIGN.md` e todas as telas em `design/telas/`. Implemente a interface fiel a elas: layout, cores (como variáveis CSS com os tokens do `DESIGN.md`), tipografia, textos em português, estados e regras de acessibilidade.

| Tela | Arquivo de referência |
|---|---|
| Principal: menu lateral com amigos e status, palco da chamada, banner de transmissão do amigo, barra de controles, chat temporário | `design/telas/Main.dc.html` |
| Compartilhar tela: tela ou janela, preset de qualidade, uso do upload, áudio, manter no modo jogo, encoder detectado | `design/telas/Compartilhar.dc.html` |
| Mesa de sons | `design/telas/Sons.dc.html` |
| Ajustes: áudio, modo jogo, atalhos, amigos (adicionar por nome e IP do Tailscale; mostrar status de identidade verificada e permitir resetar o fingerprint) | `design/telas/Ajustes.dc.html` |
| Menu da bandeja e notificação do modo jogo, feitos pelo **`sala-core`** com APIs nativas do Windows, não pelo `sala-ui` | `design/telas/ModoJogo.dc.html` |

Regras:

- Os nomes "Pedro", "Camila" e "Lucas" nas telas são exemplos: na interface real, vêm da configuração e do estado do núcleo.
- Todo número mostrado (latência, encoder, upload, tempo de chamada) deve vir do núcleo pelo IPC. Nada fixo.
- As fontes (Manrope e JetBrains Mono) são empacotadas no app como `.woff2` locais.
- Se alguma tela não cobrir um estado que você precisar (erro, carregando, sem amigos, sem conexão), crie seguindo os mesmos tokens e componentes, e liste esses estados novos no resumo final.

## 8. Metas de desempenho

Meça e documente os resultados no README:

| Métrica | Meta |
|---|---|
| RAM do `sala-core` no modo jogo, em chamada | < 50 MB |
| CPU do `sala-core` no modo jogo, em chamada | < 2% em média, num processador de 6 núcleos |
| Processos da Sala no modo jogo | apenas `sala-core` |
| Latência de voz ponta a ponta (rede local) | < 100 ms |
| Latência de tela 1080p60 (rede local) | < 150 ms |
| Tamanho do instalador | < 60 MB, incluindo as DLLs do FFmpeg |

Crie `scripts/measure.ps1`: ele coleta a RAM (working set e private bytes) e a CPU média dos processos `sala-*` durante N segundos e imprime uma tabela.

## 9. Testes e verificação

Você não terá um segundo PC. Garanta que tudo seja testável numa máquina só:

- **Perfis:** `sala-core --profile a` e `--profile b` usam diretórios de configuração e portas diferentes. Isso permite rodar **duas instâncias na mesma máquina** ligando uma para a outra via `127.0.0.1`.
- **Fontes sintéticas:** com a flag `--synthetic-audio`, o microfone é trocado por um tom senoidal. Com `--synthetic-video`, a captura de tela é trocada por um padrão de teste animado que contém um contador de quadros e um timestamp. Use isso para medir latência e perda.
- **Testes automatizados (`cargo test`):**
  - Serialização e desserialização de todas as mensagens do protocolo.
  - Fragmentação e remontagem de vídeo, incluindo perda e reordenação de fragmentos.
  - Jitter buffer com perda, atraso e reordenação simulados.
  - Mixagem da mesa de sons (sem estouro de nível: aplique limitador ou clamp).
  - Teste de integração: duas instâncias do núcleo em processo, voz sintética de A para B, verificando que B recebe ao menos 95% dos quadros.
  - Garantia de que o chat não toca o disco. Por exemplo, um teste que roda uma sessão com chat e depois verifica que nenhum arquivo do diretório de dados contém o texto enviado.
- **Detecção de encoder:** ao iniciar, logue quais encoders e decoders de hardware o FFmpeg encontrou. Crie também `sala-core --probe`, que imprime isso e sai.

## 10. Build e distribuição

- `cargo build --release` precisa funcionar a partir de um checkout limpo, seguindo o README.
- **FFmpeg:** use builds compartilhados LGPL para Windows, como os da gyan.dev ou da BtbN, **sem** componentes GPL. Documente a versão e de onde baixar, e crie `scripts/fetch-ffmpeg.ps1` para baixar e posicionar as DLLs. Se usar `libx264` (que é GPL) no fallback, isso torna o build GPL: nesse caso, documente a licença ou troque o fallback por um codec LGPL. Decida e explique a decisão.
- **Instalador** (Inno Setup, WiX ou `cargo-packager`, à sua escolha), que:
  - Instala `sala-core.exe`, `sala-ui.exe` e as DLLs.
  - Cria um atalho no Menu Iniciar apontando para `sala-core.exe`.
  - Oferece uma opção "Iniciar com o Windows".
  - Cria a regra no Firewall do Windows para a porta UDP 47800, apenas no perfil privado ou na interface do Tailscale, se for possível restringir.
- **GitHub Actions** (`windows-latest`): rodar os testes e gerar o instalador como artefato num push de tag `v*`.

## 11. README.md (em português)

Deve conter:

1. O que é o app e o que ele não faz.
2. Pré-requisitos: Tailscale instalado nos dois PCs e logado na mesma rede.
3. Como instalar e como adicionar o amigo (nome e IP do Tailscale).
4. Atalhos padrão.
5. Como funciona o modo jogo.
6. Como rodar os testes e os dois perfis numa mesma máquina.
7. Tabela de desempenho medida (seção 8).
8. Limitações conhecidas: sem cancelamento de eco, Windows apenas, mais `TODO`s relevantes.
9. Solução de problemas: firewall, Tailscale desconectado, nenhum encoder de hardware encontrado.

## 12. Critérios de aceite

O trabalho está pronto quando:

- [ ] Duas instâncias (`--profile a` e `--profile b`) numa mesma máquina conseguem: chamar, conversar por voz (voz sintética verificada por teste), trocar mensagens de chat, tocar som da mesa de sons e transmitir a tela (vídeo sintético) com o visualizador mostrando o contador.
- [ ] Entrar no modo jogo encerra `sala-ui` e o visualizador. A voz continua. Sair do modo jogo reabre a interface.
- [ ] A detecção automática de jogo funciona com um executável em tela cheia de teste.
- [ ] `cargo test` passa, e `cargo clippy -- -D warnings` passa.
- [ ] `scripts/measure.ps1` mostra o núcleo dentro das metas da seção 8 no modo jogo, e os números estão no README.
- [ ] O instalador é gerado e instala e desinstala de forma limpa.
- [ ] A interface segue `design/DESIGN.md` e as telas de `design/telas/`, incluindo as regras de acessibilidade.
- [ ] O README está completo.
- [ ] Ao terminar, você entrega um resumo com: o que foi feito, o que ficou fora ou como `TODO` e por quê, os resultados de desempenho medidos, e **o que precisa ser testado manualmente entre dois PCs reais** (conexão via Tailscale, qualidade da voz, transmissão com GPU real, impacto em um jogo real).

## 13. Fora de escopo

Não implemente: servidor de qualquer tipo, contas, histórico persistente de chat, mensagens offline, celular, macOS, Linux, overlay dentro de jogos, bots, cancelamento de eco ou atualização automática.
