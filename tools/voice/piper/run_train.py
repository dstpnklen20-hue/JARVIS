"""Запуск обучения piper1-gpl без сохранения по оценке val_mos.

Встроенный в piper.train второй ModelCheckpoint следит за val_mos (модель
UTMOS из интернета); при --model.mos_metric none метрики нет и Lightning
падает в конце первой эпохи. Оставляем только сохранение по val_mel; свои
(каждые 20 эпох) — в train_config.yaml. Аргументы — как у `python -m piper.train`.
"""

import piper.train.__main__ as piper_train

piper_train._DEFAULT_CALLBACKS[:] = piper_train._DEFAULT_CALLBACKS[:1]
piper_train.main()
