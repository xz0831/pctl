# 합성 어려운 문서 갤러리

모든 내용은 새로 만든 합성 자료입니다. 원격 주소는 example.invalid이며 가져오면 안 됩니다.

01 hangul_wrap.md: 긴 한글·NFC/NFD·혼합 문장; 원문을 보존하고 셀 폭 안에서 줄바꿈.
02 wide_chars.md: ZWJ·국기·CJK·상자·모호한 폭; 문자소를 쪼개지 않고 폭 계산.
03 nested.md: 중첩 목록·링크 강조·인용·체크·각주; 계층과 내용을 보존.
04 code.md: 언어·긴 줄·탭·문자열 및 실제 ESC; 안전한 가시 문자로 표시.
05 wide_table.md: 30열 정렬·1001열 표; 자연 폭과 가로 스크롤, 셀 내용 보존.
06 math.md: 인라인·블록·알 수 없는 매크로; 식 원문과 실패 표시 보존.
07 mermaid.md: 정상·오류·click 지시; 소스를 안전하게 표시하고 원격 동작 금지.
08 images.md + img/: 상대·누락·NFD·원격 이미지; 상대 이름 보존, 원격은 링크만.
09 headings.md: 중복·한글·긴 제목; 고유 앵커와 안정된 문단 주소.
10 links.md: 상대·공백·위험 스킴·자동 링크; 허용 스킴만 연결.
11 raw_html.md: script·onerror·iframe·details·span·제어 문자; 실행 없이 이스케이프.
12 long.md: 약 5000줄; 전체 내용과 끝 줄 보존, 시간 측정.
13 odd names/: 공백·#·%·NFD 파일 이름; 이름 바이트 보존과 정확한 경로 처리.
14 hostile.log: 실제 ANSI·OSC 52/8/0·커서·리셋·벨; 허용 스타일만 해석, 부작용 금지.
15 script.html: 인라인 및 외부 script; 격리하고 실행·원격 요청 금지.
16 script.svg: script·onload; 활성 내용 실행 금지.
17 front_matter.md: YAML 앞머리; 별도 블록으로 보존.
18 office/: DOCX 실제 표·XLSX 두 시트와 캐시 식 5·PPTX·POTX 다른 유형·한글 PDF; 구조/값/형식 보존, 실제 앱 충실도는 별도 검증.

office/parts/는 zip으로 묶은 원본 XML입니다.
