// No preset writes these rows, and changing one keeps the preset.
class ModernMenuSurfaces extends ModernMenuPage;

var localized string    ReflectionNames[4];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernReflections", 2, 3);
	for (i = 0; i < ArrayCount(ReflectionNames); i++)
		Combo.AddItem(ReflectionNames[i]);
	AddLevelSlider(1, "ModernReflectionStrength", 0, 10, 5);
	AddLevelSlider(2, "ModernMaterials", 0, 10, 5);
	AddLevelSlider(3, "ModernLampSize", 0, 10, 5);
	AddLevelSlider(4, "ModernBounceLight", 0, 10, 3);
	AddLevelSlider(5, "ModernVolumetricFog", 0, 10, 3);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Reflections and Light"
	BackHelp="Return to the Modern Graphics menu."

	ReflectionNames(0)="Off"
	ReflectionNames(1)="Captured only"
	ReflectionNames(2)="Half size"
	ReflectionNames(3)="Full size"

	Labels(0)="Reflections"
	Labels(1)="Reflection strength"
	Labels(2)="Surface materials"
	Labels(3)="Lamp size"
	Labels(4)="Bounce light"
	Labels(5)="Lamp lit fog"

	Helps(0)="Floors, glass and water reflect the scene. Captured only uses fixed pictures of each room; full size costs the most."
	Helps(1)="How strongly a reflecting surface mirrors. 0 turns it off."
	Helps(2)="Metal, wood, stone and the rest each get their own roughness and shine. 0 turns it off."
	Helps(3)="Lamps light as wide sources sized from their glow and fixture. 0 keeps them as points."
	Helps(4)="Light bounces off what is on screen into nearby shade. 0 turns it off."
	Helps(5)="Light from lamps glows in the air around them. 0 turns it off."
}
